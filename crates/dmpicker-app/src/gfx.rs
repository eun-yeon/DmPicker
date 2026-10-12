//! DirectComposition 위 Direct2D 기반: 팝업 창의 픽셀별 알파 서피스, 효과 도우미, 텍스트 형식, 아이콘,
//! 검색창과 알림이 같이 쓰는 색과 그림자

use windows::core::{Error, Interface, Result, HSTRING};
use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HWND};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

use crate::icons::{Icon, VIEW};

// 색: 어두운 판 위에 불투명도만 다른 흰색
// 글자·아이콘·채움이 판처럼 배경색을 띠고, 배경이 얼마나 밝든 진하기 순서가 유지됨
const fn ink(a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a }
}
/// 기본 글자: 입력한 글, 제목, 곡 이름, 레벨
pub const TEXT: D2D1_COLOR_F = ink(0.96);
/// 보조: 라벨, 아랫줄, 평소 아이콘
pub const SUB: D2D1_COLOR_F = ink(0.70);
/// 3순위: 안내 문구, 메모, 없는 레벨
/// 11-12 px 글자가 읽히는 가장 흐린 값 (판 뒤가 가장 밝아도 4.5:1 이상)
pub const FAINT: D2D1_COLOR_F = ink(0.55);
/// 4순위: 고를 수 없는 줄의 글자, 재킷도 이만큼 흐리게
pub const DISABLED: D2D1_COLOR_F = ink(0.32);
/// 커서: 순백, 옆 글자보다 한 단계 밝게 (1.5 px 선은 더 흐려 보임)
pub const CARET: D2D1_COLOR_F = ink(1.0);
/// 채움 불투명도, 흐린 순으로 TRACK 세그먼트 컨트롤 트랙, HOVER 호버,
/// SELECTED 고른 줄·평소 버튼, STRONG 호버한 버튼·세그먼트 손잡이
pub const FILL_TRACK: f32 = 0.06;
pub const FILL_HOVER: f32 = 0.10;
pub const FILL_SELECTED: f32 = 0.14;
pub const FILL_STRONG: f32 = 0.20;
/// 판: 검색창과 알림 바탕, 이 색을 `PLATE_ALPHA`로 칠해 뒤의 게임이 살짝 비침
pub const TINT: D2D1_COLOR_F = D2D1_COLOR_F { r: 0.090, g: 0.098, b: 0.114, a: 1.0 }; // #17191D
pub const PLATE_ALPHA: f32 = 0.92;
/// 즐겨찾기: 켜진 별의 테두리·채움과 켤 때 튀는 점
pub const FAVORITE: D2D1_COLOR_F = D2D1_COLOR_F { r: 1.0, g: 0.788, b: 0.290, a: 1.0 }; // #FFC94A

/// 글리프 왼쪽에 물리 px로 1쯤 여백이 있음, 그만큼 앞당겨 그려야 글자가 정렬선에 맞음
pub const BEARING: f32 = 1.0;

/// 애니메이션 프레임 상한, 프레임은 그래도 모니터 주사율에 맞춰 나감 (`Gfx::sync`)
/// 검색창: 240 Hz까지는 주사율 그대로, CPU가 fps에 비례해 늘어서 더 빠른 모니터는 나눠서 이 아래로
pub const MAX_FPS: u32 = 240;
/// 알림: 곡 인식 중에도 계속 그려서, 키 입력이 게임 프레임에 기대는 인식이 게임과 CPU를 덜 다투게 낮춤
pub const TOAST_MAX_FPS: u32 = 120;

/// UI 글꼴, exe에 넣어 둬서 설치 안 된 PC에서도 같은 글꼴로 보임
/// 쓰는 굵기 (400 · 500 · 600)만, 배포본 woff2 그대로 (라이선스 licenses/Pretendard-OFL.txt)
const UI_FAMILY: &str = "Pretendard";
static UI_FONTS: [&[u8]; 3] = [
    include_bytes!("../assets/fonts/Pretendard-Regular.woff2"),
    include_bytes!("../assets/fonts/Pretendard-Medium.woff2"),
    include_bytes!("../assets/fonts/Pretendard-SemiBold.woff2"),
];

thread_local! {
    // 검색창과 알림이 같이 씀, 압축은 처음 한 번만 풂
    static UI_COLLECTION: std::cell::OnceCell<Option<IDWriteFontCollection>> = const { std::cell::OnceCell::new() };
}

pub struct Gfx {
    pub factory: ID2D1Factory1,
    dwrite: IDWriteFactory,
    pub dc: ID2D1DeviceContext,
    /// 못 읽었으면 None, 설치된 글꼴로 대신 그림
    ui_fonts: Option<IDWriteFontCollection>,
    swapchain: IDXGISwapChain1,
    _dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    /// 프레임당 vblank 수, 모니터 주사율을 나눠 `max_fps` 이하로 맞춤
    /// 모든 프레임이 같은 시간 보임 (240 Hz에서 타이머로 60 fps를 맞출 때 생기는 4-5-4 떨림 없음)
    sync: std::cell::Cell<u32>,
    max_fps: u32,
    // 애니메이션 중엔 프레임마다 객체를 새로 만드는 게 CPU를 제일 많이 써서 캐시함
    brushes: std::cell::RefCell<std::collections::HashMap<[u32; 4], ID2D1SolidColorBrush>>,
    layouts: std::cell::RefCell<std::collections::HashMap<(usize, String, u32, u32), IDWriteTextLayout>>,
    icons: std::cell::RefCell<std::collections::HashMap<Icon, ID2D1PathGeometry1>>,
}

impl Gfx {
    /// `max_fps`: [`MAX_FPS`] (검색창) 또는 [`TOAST_MAX_FPS`] (알림)
    pub fn new(hwnd: HWND, width: u32, height: u32, max_fps: u32) -> Result<Self> {
        unsafe {
            let mut d3d: Option<ID3D11Device> = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                Default::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                None,
                None,
            )?;
            let d3d = d3d.unwrap();
            let dxgi_device: IDXGIDevice = d3d.cast()?;
            // 대기 프레임 1개, Present가 화면을 기다려서 각 프레임이 실제로 보일 vblank 기준으로 계산됨
            // 애니메이션을 시작할 때 쌓인 프레임이 몰려 나가지 않음
            dxgi_device.cast::<IDXGIDevice1>()?.SetMaximumFrameLatency(1)?;

            let factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let d2d_device = factory.CreateDevice(&dxgi_device)?;
            let dc = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;

            let dxgi_factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: width,
                Height: height,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
                ..Default::default()
            };
            let swapchain = dxgi_factory.CreateSwapChainForComposition(&dxgi_device, &desc, None)?;

            let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi_device)?;
            let target = dcomp.CreateTargetForHwnd(hwnd, true)?;
            let visual = dcomp.CreateVisual()?;
            visual.SetContent(&swapchain)?;
            target.SetRoot(&visual)?;
            dcomp.Commit()?;

            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let ui_fonts = UI_COLLECTION.with(|c| {
                c.get_or_init(|| load_ui_fonts(&dwrite).inspect_err(|e| eprintln!("ui font load failed: {e:?}")).ok()).clone()
            });
            let g = Self {
                factory,
                dwrite,
                dc,
                ui_fonts,
                swapchain,
                _dcomp: dcomp,
                _target: target,
                _visual: visual,
                sync: std::cell::Cell::new(1),
                max_fps,
                brushes: Default::default(),
                layouts: Default::default(),
                icons: Default::default(),
            };
            g.bind_target()?;
            g.retune(hwnd);
            Ok(g)
        }
    }

    /// 창이 있는 모니터의 주사율을 다시 읽음 (창을 띄울 때 호출)
    pub fn retune(&self, hwnd: HWND) {
        // Present는 1..=4만 받음 (주사율이 상한의 4배를 넘으면 상한보다 조금 빨라짐)
        self.sync.set(crate::sys::refresh_hz(hwnd).div_ceil(self.max_fps).clamp(1, 4));
    }

    fn bind_target(&self) -> Result<()> {
        unsafe {
            let surface: IDXGISurface = self.swapchain.GetBuffer(0)?;
            let props = bitmap_props(D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW);
            let bmp = self.dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
            self.dc.SetTarget(&bmp);
        }
        Ok(())
    }

    pub fn present(&self) -> Result<()> {
        unsafe { self.swapchain.Present(self.sync.get(), DXGI_PRESENT(0)).ok() }
    }

    /// straight RGBA 픽셀로 premultiplied BGRA 비트맵을 만듦
    pub fn bitmap_from_rgba(&self, w: u32, h: u32, rgba: &[u8]) -> Result<ID2D1Bitmap1> {
        let mut bgra = vec![0u8; rgba.len()];
        for (d, s) in bgra.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
            let a = s[3] as u32;
            d[0] = ((s[2] as u32 * a + 127) / 255) as u8;
            d[1] = ((s[1] as u32 * a + 127) / 255) as u8;
            d[2] = ((s[0] as u32 * a + 127) / 255) as u8;
            d[3] = s[3];
        }
        self.bitmap_from_bgra(w, h, &bgra)
    }

    pub fn bitmap_from_bgra(&self, w: u32, h: u32, bgra: &[u8]) -> Result<ID2D1Bitmap1> {
        let props = bitmap_props(D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_BITMAP_OPTIONS_NONE);
        unsafe { self.dc.CreateBitmap(D2D_SIZE_U { width: w, height: h }, Some(bgra.as_ptr() as _), w * 4, &props) }
    }

    pub fn effect(&self, clsid: &windows::core::GUID) -> Result<ID2D1Effect> {
        unsafe { self.dc.CreateEffect(clsid) }
    }

    pub fn brush(&self, c: D2D1_COLOR_F) -> Result<ID2D1SolidColorBrush> {
        let key = [c.r.to_bits(), c.g.to_bits(), c.b.to_bits(), c.a.to_bits()];
        if let Some(b) = self.brushes.borrow().get(&key) {
            return Ok(b.clone());
        }
        let b = unsafe { self.dc.CreateSolidColorBrush(&c, None)? };
        let mut m = self.brushes.borrow_mut();
        if m.len() > 512 {
            m.clear();
        }
        m.insert(key, b.clone());
        Ok(b)
    }

    /// `from`에서 `to`로 가는 선형 그라데이션
    pub fn gradient(&self, stops: &[D2D1_GRADIENT_STOP], from: windows_numerics::Vector2, to: windows_numerics::Vector2) -> Result<ID2D1LinearGradientBrush> {
        unsafe {
            let rt: &ID2D1RenderTarget = &self.dc;
            let coll = rt.CreateGradientStopCollection(stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
            self.dc.CreateLinearGradientBrush(&D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: from, endPoint: to }, None, &coll)
        }
    }

    /// 판 테두리 선의 빛: 위는 밝게, 옆은 흐리게, 아래는 은은하게 (검색창과 알림이 같음)
    pub fn rim_gradient(&self, top: f32, bottom: f32, alpha: f32) -> Result<ID2D1LinearGradientBrush> {
        let stops = [(0.0, 0.22), (0.3, 0.10), (1.0, 0.06)].map(|(position, a)| D2D1_GRADIENT_STOP { position, color: white(a * alpha) });
        self.gradient(&stops, pt(0.0, top), pt(0.0, bottom))
    }

    /// 아이콘을 가운데 (`cx`, `cy`)에 한 변 `size`인 상자로 채움, 경로는 처음 한 번만 만듦
    pub fn fill_icon(&self, icon: Icon, cx: f32, cy: f32, size: f32, brush: &ID2D1SolidColorBrush) -> Result<()> {
        let cached = self.icons.borrow().get(&icon).cloned();
        let path = match cached {
            Some(p) => p,
            None => {
                let p = crate::icons::geometry(&self.factory, icon)?;
                self.icons.borrow_mut().insert(icon, p.clone());
                p
            }
        };
        let k = size / VIEW;
        let place = windows_numerics::Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: cx - VIEW / 2.0 * k, M32: cy - VIEW / 2.0 * k };
        unsafe {
            let mut xf = windows_numerics::Matrix3x2::identity();
            self.dc.GetTransform(&mut xf);
            self.dc.SetTransform(&(place * xf));
            self.dc.FillGeometry(&path, brush, None);
            self.dc.SetTransform(&xf);
        }
        Ok(())
    }

    /// UI 글자 형식, 넣어 둔 프리텐다드로 그림
    pub fn ui_format(&self, size: f32, weight: DWRITE_FONT_WEIGHT) -> Result<IDWriteTextFormat> {
        unsafe {
            let f = self.dwrite.CreateTextFormat(
                &HSTRING::from(UI_FAMILY),
                self.ui_fonts.as_ref(),
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                &HSTRING::from("ko-kr"),
            )?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            let sign = self.dwrite.CreateEllipsisTrimmingSign(&f)?;
            f.SetTrimming(&trimming, &sign)?;
            Ok(f)
        }
    }

    /// 캐시된 텍스트 레이아웃, 글·형식·박스가 같으면 같은 객체
    /// 호출하는 쪽에서 정렬·범위를 매 프레임 다시 적용해도 가벼움
    pub fn layout(&self, text: &str, fmt: &IDWriteTextFormat, max_w: f32, max_h: f32) -> Result<IDWriteTextLayout> {
        let key = (fmt.as_raw() as usize, text.to_string(), max_w.to_bits(), max_h.to_bits());
        if let Some(l) = self.layouts.borrow().get(&key) {
            return Ok(l.clone());
        }
        let l = self.layout_fresh(text, fmt, max_w, max_h)?;
        let mut m = self.layouts.borrow_mut();
        if m.len() > 800 {
            m.clear();
        }
        m.insert(key, l.clone());
        Ok(l)
    }

    /// 박스 가운데에 맞춘 캐시 레이아웃
    pub fn layout_centered(&self, text: &str, fmt: &IDWriteTextFormat, max_w: f32, max_h: f32) -> Result<IDWriteTextLayout> {
        let l = self.layout(text, fmt, max_w, max_h)?;
        unsafe {
            l.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            l.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        }
        Ok(l)
    }

    /// 캐시 안 하는 레이아웃, 범위별 스타일이 프레임마다 바뀌는 글에 씀
    pub fn layout_fresh(&self, text: &str, fmt: &IDWriteTextFormat, max_w: f32, max_h: f32) -> Result<IDWriteTextLayout> {
        let w: Vec<u16> = text.encode_utf16().collect();
        unsafe { self.dwrite.CreateTextLayout(&w, fmt, max_w, max_h) }
    }
}

/// 판 그림자: 검정 28 %, 표준편차 12 DIP로 흐려 6 DIP 아래로 (검색창과 알림이 같음)
const SHADOW_ALPHA: f32 = 0.28;
const SHADOW_BLUR: f32 = 12.0;
const SHADOW_DROP: f32 = 6.0;

/// 판 그림자 효과, 입력은 판 모양을 흰색으로 채운 마스크
/// 출력은 입력과 같은 좌표라 변환 없이 판보다 먼저 그림
pub struct Shadow {
    shadow: ID2D1Effect,
    xf: ID2D1Effect,
}

impl Shadow {
    pub fn new(g: &Gfx, scale: f32) -> Result<Self> {
        let shadow = g.effect(&CLSID_D2D1Shadow)?;
        set_f32(&shadow, D2D1_SHADOW_PROP_BLUR_STANDARD_DEVIATION.0 as u32, SHADOW_BLUR * scale)?;
        set_floats(&shadow, D2D1_SHADOW_PROP_COLOR.0 as u32, &[0.0, 0.0, 0.0, SHADOW_ALPHA])?;
        let xf = g.effect(&CLSID_D2D12DAffineTransform)?;
        set_floats(&xf, D2D1_2DAFFINETRANSFORM_PROP_TRANSFORM_MATRIX.0 as u32, &[1.0, 0.0, 0.0, 1.0, 0.0, SHADOW_DROP * scale])?;
        set_input_effect(&xf, 0, &shadow)?;
        Ok(Self { shadow, xf })
    }

    /// 모양을 효과 출력으로 받음 (검색창의 모양 마스크)
    pub fn set_input_effect(&self, shape: &ID2D1Effect) -> Result<()> {
        set_input_effect(&self.shadow, 0, shape)
    }

    /// 모양을 이미지로 받음 (알림 캡슐을 채운 커맨드 리스트)
    pub fn set_input_image(&self, shape: &ID2D1Image) {
        set_input_image(&self.shadow, 0, shape)
    }

    pub fn output(&self) -> Result<ID2D1Image> {
        unsafe { self.xf.GetOutput() }
    }
}

/// 그래픽 장치가 사라진 오류 (드라이버 업데이트, TDR), 장치부터 새로 만들어야 다시 그려짐
pub fn device_lost(e: &Error) -> bool {
    [DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, D2DERR_RECREATE_TARGET].contains(&e.code())
}

/// 꼭 끝나는 `BeginDraw`, `?`로 일찍 나가도 drop에서 끝냄
/// 그때 EndDraw가 오류로 남은 레이어·클립을 정리함
/// 그동안은 `xf` 변환으로 그리고 끝나면 단위 변환으로 돌아감
pub struct Drawing<'a>(&'a ID2D1DeviceContext);

impl<'a> Drawing<'a> {
    pub fn begin(dc: &'a ID2D1DeviceContext, xf: &windows_numerics::Matrix3x2) -> Self {
        unsafe {
            dc.BeginDraw();
            dc.SetTransform(xf);
        }
        Self(dc)
    }

    pub fn end(self) -> Result<()> {
        let dc = self.0;
        std::mem::forget(self);
        unsafe {
            let r = dc.EndDraw(None, None);
            dc.SetTransform(&windows_numerics::Matrix3x2::identity());
            r
        }
    }
}

impl Drop for Drawing<'_> {
    fn drop(&mut self) {
        unsafe {
            let _ = self.0.EndDraw(None, None);
            self.0.SetTransform(&windows_numerics::Matrix3x2::identity());
        }
    }
}

/// drop될 때 컨텍스트 타깃을 `.1`로 되돌림
/// 커맨드 리스트에 기록하다 `?`로 일찍 나가도 다음 프레임은 창에 그려짐
pub struct Retarget<'a>(pub &'a ID2D1DeviceContext, pub ID2D1Image);

impl Drop for Retarget<'_> {
    fn drop(&mut self) {
        unsafe { self.0.SetTarget(&self.1) };
    }
}

/// exe에 넣어 둔 woff2를 풀어 글꼴 모음으로 만듦
fn load_ui_fonts(dwrite: &IDWriteFactory) -> Result<IDWriteFontCollection> {
    unsafe {
        let f: IDWriteFactory5 = dwrite.cast()?;
        let loader = f.CreateInMemoryFontFileLoader()?;
        f.RegisterFontFileLoader(&loader)?;
        let set = f.CreateFontSetBuilder()?;
        for data in UI_FONTS {
            let stream = f.UnpackFontFile(DWRITE_CONTAINER_TYPE_WOFF2, data.as_ptr() as _, data.len() as u32)?;
            let size = stream.GetFileSize()?;
            let (mut ptr, mut ctx) = (std::ptr::null_mut(), std::ptr::null_mut());
            stream.ReadFileFragment(&mut ptr, 0, size, &mut ctx)?;
            // 주인 객체 없이 넘기면 DirectWrite가 복사해 둠, 스트림은 바로 버려도 됨
            let file = loader.CreateInMemoryFontFileReference(&f, ptr, size as u32, None);
            stream.ReleaseFileFragment(ctx);
            set.AddFontFile(&file?)?;
        }
        f.CreateFontCollectionFromFontSet(&set.CreateFontSet()?)?.cast()
    }
}

/// 96 dpi BGRA 비트맵 속성
fn bitmap_props(alpha: D2D1_ALPHA_MODE, options: D2D1_BITMAP_OPTIONS) -> D2D1_BITMAP_PROPERTIES1 {
    D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: alpha },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: options,
        colorContext: std::mem::ManuallyDrop::new(None),
    }
}

pub fn color(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

pub fn white(a: f32) -> D2D1_COLOR_F {
    color(1.0, 1.0, 1.0, a)
}

pub fn with_alpha(mut c: D2D1_COLOR_F, a: f32) -> D2D1_COLOR_F {
    c.a *= a;
    c
}

pub fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

pub fn rrect(l: f32, t: f32, r: f32, b: f32, rad: f32) -> D2D1_ROUNDED_RECT {
    rounded(rect(l, t, r, b), rad)
}

pub fn rounded(r: D2D_RECT_F, rad: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: r, radiusX: rad, radiusY: rad }
}

pub fn circle(x: f32, y: f32, r: f32) -> D2D1_ELLIPSE {
    D2D1_ELLIPSE { point: pt(x, y), radiusX: r, radiusY: r }
}

pub fn pt(x: f32, y: f32) -> windows_numerics::Vector2 {
    windows_numerics::Vector2 { X: x, Y: y }
}

pub fn set_f32(e: &ID2D1Effect, idx: u32, v: f32) -> Result<()> {
    unsafe { e.SetValue(idx, D2D1_PROPERTY_TYPE_UNKNOWN, &v.to_le_bytes()) }
}

pub fn set_u32(e: &ID2D1Effect, idx: u32, v: u32) -> Result<()> {
    unsafe { e.SetValue(idx, D2D1_PROPERTY_TYPE_UNKNOWN, &v.to_le_bytes()) }
}

pub fn set_bool(e: &ID2D1Effect, idx: u32, v: bool) -> Result<()> {
    set_u32(e, idx, v as u32)
}

/// 벡터나 행렬 속성, 행렬은 D2D가 받는 대로 행 우선
/// 5x4 색 행렬은 [r,g,b,a] 곱수 5행
pub fn set_floats(e: &ID2D1Effect, idx: u32, v: &[f32]) -> Result<()> {
    let b: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
    unsafe { e.SetValue(idx, D2D1_PROPERTY_TYPE_UNKNOWN, &b) }
}

pub fn set_input_effect(e: &ID2D1Effect, idx: u32, input: &ID2D1Effect) -> Result<()> {
    unsafe {
        let out = input.GetOutput()?;
        e.SetInput(idx, &out, true);
    }
    Ok(())
}

pub fn set_input_image(e: &ID2D1Effect, idx: u32, img: &ID2D1Image) {
    unsafe { e.SetInput(idx, img, true) }
}
