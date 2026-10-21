//! 창을 윈도우 합성기 (Windows.UI.Composition)에 올림
//! 그린 화면 (swapchain) 한 장 아래위로 빛 비주얼을 겹침, 빛의 움직임은 합성기가 화면 주사율로 그림
//! 윈도우 11에선 맨 아래에 판 모양으로 자른 흐린 뒤 화면을 깖 (유리 효과)
//! 합성기를 못 만들면 `Gfx`가 DirectComposition으로 그린 화면만 올림 (빛 없음)

use std::cell::{Cell, OnceCell};

use windows::core::{s, w, Interface, Result, BOOL, HRESULT, HSTRING};
use windows::Graphics::Effects::IGraphicsEffectSource;
use windows::System::DispatcherQueueController;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    CompositionEffectSourceParameter, CompositionGraphicsDevice, CompositionStretch, CompositionSurfaceBrush, Compositor, ContainerVisual,
    ICompositionSurface, SpriteVisual, VisualCollection,
};
use windows::Win32::Foundation::{E_FAIL, HWND};
use windows::Win32::Graphics::Direct2D::{ID2D1Device, CLSID_D2D1Saturation};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_HOSTBACKDROPBRUSH};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain1;
use windows::Win32::System::WinRT::Composition::{ICompositorDesktopInterop, ICompositorInterop};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};
use windows::Win32::System::WinRT::{DispatcherQueueOptions, DQTAT_COM_NONE, DQTYPE_THREAD_CURRENT};
use windows_numerics::{Matrix3x2, Matrix4x4, Vector2};

use crate::beam::{Fx, Prop};

/// 흐린 뒤 화면에 올리는 채도, 흐리면 색이 바래 보임
const SATURATION: f32 = 1.25;

thread_local! {
    // UI 스레드에 하나, 검색창과 알림이 같이 씀 (못 만들면 None)
    static COMPOSITOR: OnceCell<Option<(DispatcherQueueController, Compositor)>> = const { OnceCell::new() };
}

/// UI 스레드의 합성기, 처음 부를 때 만듦 (합성기 일은 이 스레드의 메시지 루프가 돌림)
fn compositor() -> Option<Compositor> {
    COMPOSITOR.with(|c| {
        c.get_or_init(|| {
            let make = || -> Result<(DispatcherQueueController, Compositor)> { Ok((dispatcher_queue()?, Compositor::new()?)) };
            make().inspect_err(|e| eprintln!("compositor: {e:?}")).ok()
        })
        .as_ref()
        .map(|(_, c)| c.clone())
    })
}

/// 이 스레드의 디스패처 큐, 만드는 함수는 실행 중에 찾음
/// 1709 이전 윈도우엔 없는 함수라 바로 링크하면 앱이 안 켜짐
fn dispatcher_queue() -> Result<DispatcherQueueController> {
    type Create = unsafe extern "system" fn(DispatcherQueueOptions, *mut *mut std::ffi::c_void) -> HRESULT;
    unsafe {
        let lib = LoadLibraryExW(w!("CoreMessaging.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)?;
        let found = GetProcAddress(lib, s!("CreateDispatcherQueueController")).ok_or_else(|| windows::core::Error::from(E_FAIL))?;
        let create: Create = std::mem::transmute(found);
        let options = DispatcherQueueOptions {
            dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_NONE,
        };
        let mut queue = std::ptr::null_mut();
        create(options, &mut queue).ok()?;
        Ok(DispatcherQueueController::from_raw(queue))
    }
}

pub struct Scene {
    pub compositor: Compositor,
    target: DesktopWindowTarget,
    _root: ContainerVisual,
    /// 그린 화면 아래 (판 밖으로 번지는 빛)와 위 (테두리 빛), 둘 다 창 좌표 (px)
    pub under: ContainerVisual,
    pub over: ContainerVisual,
    /// 한 번 그려 두는 빛 모양을 Direct2D로 그릴 장치
    pub graphics: CompositionGraphicsDevice,
    /// 맨 아래 흐림 층, 윈도우 11이 아니거나 못 만들면 None (판만 그림)
    glass: Option<Glass>,
}

/// 판 뒤에 비치는 흐린 화면: 윈도우가 흐린 창 뒤 화면에 채도를 올리고 판 모양 마스크로 자름
/// 마스크는 그린 화면 아래 칸, 앱이 판과 같은 모양·불투명도로 그려 넣음 (`Gfx::paint_mask`)
struct Glass {
    visual: SpriteVisual,
    shown: Cell<f32>, // 마지막으로 넣은 불투명도, 같으면 합성기를 안 건드림
}

impl Glass {
    /// 창 크기 `w`×`h` (px) 층을 `layers` 맨 아래에 숨긴 채로 붙임, `surface`는 높이 2`h`인 그린 화면
    fn new(compositor: &Compositor, layers: &VisualCollection, surface: &ICompositionSurface, hwnd: HWND, w: f32, h: f32) -> Result<Self> {
        // 이 창 속성이 없으면 뒤 화면 대신 단색이 나옴
        let on = BOOL(1);
        unsafe { DwmSetWindowAttribute(hwnd, DWMWA_USE_HOSTBACKDROPBRUSH, &on as *const BOOL as _, std::mem::size_of::<BOOL>() as u32)? };
        let param = HSTRING::from("backdrop");
        let source: IGraphicsEffectSource = CompositionEffectSourceParameter::Create(&param)?.cast()?;
        let saturate = Fx::make("Saturation", CLSID_D2D1Saturation, vec![Prop::F32(SATURATION)], vec![], vec![source]);
        let backdrop = compositor.CreateEffectFactory(&saturate)?.CreateBrush()?;
        backdrop.SetSourceParameter(&param, &compositor.CreateHostBackdropBrush()?)?;
        let masked = compositor.CreateMaskBrush()?;
        masked.SetSource(&backdrop)?;
        masked.SetMask(&unstretched(compositor, surface, 1.0)?)?;
        let visual = compositor.CreateSpriteVisual()?;
        visual.SetSize(Vector2 { X: w, Y: h })?;
        visual.SetBrush(&masked)?;
        visual.SetIsVisible(false)?;
        layers.InsertAtBottom(&visual)?;
        Ok(Self { visual, shown: Cell::new(0.0) })
    }
}

/// 늘이지 않은 swapchain 붓, 높이가 창의 2배면 `align_y` 0은 위 칸 (그린 화면), 1은 아래 칸 (마스크)만 보임
fn unstretched(compositor: &Compositor, surface: &ICompositionSurface, align_y: f32) -> Result<CompositionSurfaceBrush> {
    let brush = compositor.CreateSurfaceBrushWithSurface(surface)?;
    brush.SetStretch(CompositionStretch::None)?;
    brush.SetHorizontalAlignmentRatio(0.0)?;
    brush.SetVerticalAlignmentRatio(align_y)?;
    Ok(brush)
}

impl Scene {
    /// 그린 화면을 가운데 층으로 쌓고 맨 마지막에 창에 붙임
    /// 중간에 실패하면 창은 그대로라 DirectComposition으로 올릴 수 있음
    /// `glass`면 swapchain 아래 칸이 유리 효과 마스크
    pub fn new(hwnd: HWND, swapchain: &IDXGISwapChain1, device: &ID2D1Device, w: f32, h: f32, glass: bool) -> Result<Self> {
        let compositor = compositor().ok_or_else(|| windows::core::Error::from(E_FAIL))?;
        let size = Vector2 { X: w, Y: h };
        unsafe {
            let root = compositor.CreateContainerVisual()?;
            root.SetSize(size)?;
            let interop: ICompositorInterop = compositor.cast()?;
            let surface = interop.CreateCompositionSurfaceForSwapChain(swapchain)?;
            let content = compositor.CreateSpriteVisual()?;
            content.SetSize(size)?;
            content.SetBrush(&unstretched(&compositor, &surface, 0.0)?)?;
            let under = compositor.CreateContainerVisual()?;
            let over = compositor.CreateContainerVisual()?;
            for v in [&under, &over] {
                v.SetSize(size)?;
            }
            let layers = root.Children()?;
            layers.InsertAtTop(&under)?;
            layers.InsertAtTop(&content)?;
            layers.InsertAtTop(&over)?;
            let graphics = interop.CreateGraphicsDevice(device)?;
            // 흐림 층을 못 만들어도 빛과 그린 화면은 그대로 올림
            let glass = if glass { Glass::new(&compositor, &layers, &surface, hwnd, w, h).inspect_err(|e| eprintln!("glass: {e:?}")).ok() } else { None };
            let target = compositor.cast::<ICompositorDesktopInterop>()?.CreateDesktopWindowTarget(hwnd, true)?;
            target.SetRoot(&root)?;
            Ok(Self { compositor, target, _root: root, under, over, graphics, glass })
        }
    }

    /// 빛 층 전체에 변환 (검색창이 닫히며 한 덩어리로 줄어들 때)
    pub fn set_transform(&self, xf: &Matrix3x2) -> Result<()> {
        let m = matrix4(xf);
        self.under.SetTransformMatrix(m)?;
        self.over.SetTransformMatrix(m)
    }

    pub fn has_glass(&self) -> bool {
        self.glass.is_some()
    }

    /// 판 뒤 흐린 화면을 `glass` (0..1)만큼 보임, 0이면 숨김, 흐림 층이 없으면 아무것도 안 함
    pub fn set_glass(&self, glass: f32) -> Result<()> {
        let Some(gl) = &self.glass else { return Ok(()) };
        if gl.shown.get() != glass {
            gl.shown.set(glass);
            gl.visual.SetOpacity(glass)?;
            gl.visual.SetIsVisible(glass > 0.0)?;
        }
        Ok(())
    }
}

impl Drop for Scene {
    /// 창에서 바로 떼어 냄, 같은 창에 새로 붙일 수 있게 (장치를 다시 만들 때)
    fn drop(&mut self) {
        let _ = self.target.Close();
    }
}

/// 평면 변환을 비주얼 변환으로
pub fn matrix4(xf: &Matrix3x2) -> Matrix4x4 {
    Matrix4x4 {
        M11: xf.M11,
        M12: xf.M12,
        M13: 0.0,
        M14: 0.0,
        M21: xf.M21,
        M22: xf.M22,
        M23: 0.0,
        M24: 0.0,
        M31: 0.0,
        M32: 0.0,
        M33: 1.0,
        M34: 0.0,
        M41: xf.M31,
        M42: xf.M32,
        M43: 0.0,
        M44: 1.0,
    }
}
