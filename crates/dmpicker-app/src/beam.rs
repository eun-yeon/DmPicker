//! 판 테두리를 따라 움직이는 빛, 움직임은 합성기가 화면 주사율로 그리고 앱은 켜고 끄고 자리만 알려 줌
//! - `Kind::Spin`: 흰 빛이 테두리를 돎 (검색창을 열 때 한 바퀴, 곡 목록 인식 중 알림)
//! - `Kind::Line`: 빛이 아래 테두리를 좌우로 오감 (목록 인식 카드는 흰색, 무작위로 고르기 줄은 여러 색)
//! - `Kind::Glow`: 판 바깥으로 빛이 번졌다 사라짐 (인식 결과 알림, 완료는 초록, 실패는 빨강)
//!
//! 겹마다 빛 조각 (타원 그라데이션)을 한 칸에 모아 그린 뒤 마스크로 가림
//! 마스크는 한 번 그려 둔 모양 (테두리 고리, 가장자리 띠)에 빛 자리를 따라 움직이는 가림을 곱한 것
//! 조각의 자리·크기는 속성 집합 값을 따라가는 식으로 묶임

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use windows::core::{implement, Interface, Result, GUID, HSTRING, PCWSTR};
use windows::Foundation::{IPropertyValue, PropertyValue, Size, TimeSpan};
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::Graphics::Effects::{IGraphicsEffect, IGraphicsEffect_Impl, IGraphicsEffectSource, IGraphicsEffectSource_Impl};
use windows::UI::Color;
use windows::UI::Composition::*;
use windows::Win32::Foundation::{E_INVALIDARG, POINT};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::System::WinRT::Composition::ICompositionDrawingSurfaceInterop;
use windows::Win32::System::WinRT::Graphics::Direct2D::{
    IGraphicsEffectD2D1Interop, IGraphicsEffectD2D1Interop_Impl, GRAPHICS_EFFECT_PROPERTY_MAPPING, GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT,
};
use windows_numerics::{Matrix3x2, Vector2, Vector3};

use crate::gfx::{self, rect, rounded};
use crate::scene::Scene;

/// 나타나는 시간, 사라지는 시간 (초)
const FADE_IN: f32 = 0.6;
const FADE_OUT: f32 = 0.5;
/// 도는 빛이 한 바퀴 도는 초
pub const SPIN_SECS: f32 = 1.96;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Spin,
    Line { colorful: bool },
    Glow { failed: bool },
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum State {
    Off,
    On,
    /// 이 시각부터 사라짐 (끔, 또는 한 번 보여 주기가 끝남), 다 사라지면 `settle`이 움직임을 멈춤
    Fading(Instant),
}

/// 빛 하나: 창의 위 층 (`Kind::Glow`는 아래 층에도)에 붙은 비주얼과 그 움직임
pub struct Beam {
    compositor: Compositor,
    over: ContainerVisual,
    under: Option<ContainerVisual>,
    parents: (ContainerVisual, ContainerVisual),
    props: CompositionPropertySet,
    /// 켤 때 다시 거는 움직임 (대상, 속성 이름, 애니메이션)
    motion: Vec<(CompositionObject, HSTRING, CompositionAnimation)>,
    /// 만든 크기 (px), 다른 크기에 놓으면 늘려서 맞춤
    size: (f32, f32),
    state: Cell<State>,
    alpha: Cell<f32>,
}

impl Drop for Beam {
    fn drop(&mut self) {
        let _ = self.parents.0.Children().and_then(|c| c.Remove(&self.over));
        if let Some(u) = &self.under {
            let _ = self.parents.1.Children().and_then(|c| c.Remove(u));
        }
    }
}

impl Beam {
    /// `w`×`h` (px) 모서리 `radius`인 판에 씌울 빛, `s`는 DIP당 px
    pub fn new(scene: &Scene, kind: Kind, w: f32, h: f32, radius: f32, s: f32) -> Result<Self> {
        let c = &scene.compositor;
        let props = c.CreatePropertySet()?;
        props.InsertScalar(&HSTRING::from("fade"), 0.0)?;
        props.InsertScalar(&HSTRING::from("alpha"), 1.0)?;
        let m = Make { c, graphics: &scene.graphics, props: &props, w, h, s, motion: RefCell::new(Vec::new()) };
        let over = c.CreateContainerVisual()?;
        over.SetSize(Vector2 { X: w, Y: h })?;
        let under = match kind {
            Kind::Spin => {
                m.spin(&over, radius)?;
                None
            }
            Kind::Line { colorful } => {
                m.line(&over, radius, colorful)?;
                None
            }
            Kind::Glow { failed } => {
                let under = c.CreateContainerVisual()?;
                under.SetSize(Vector2 { X: w, Y: h })?;
                m.glow(&over, &under, radius, failed)?;
                Some(under)
            }
        };
        over.SetIsVisible(false)?;
        scene.over.Children()?.InsertAtTop(&over)?;
        if let Some(u) = &under {
            u.SetIsVisible(false)?;
            scene.under.Children()?.InsertAtTop(u)?;
        }
        let motion = m.motion.into_inner();
        Ok(Self {
            compositor: c.clone(),
            over,
            under,
            parents: (scene.over.clone(), scene.under.clone()),
            props,
            motion,
            size: (w, h),
            state: Cell::new(State::Off),
            alpha: Cell::new(1.0),
        })
    }

    /// 판이 지금 있는 자리 (창 좌표 px), 만든 크기와 다르면 늘려서 맞춤
    /// `alpha`는 판의 불투명도 (열고 닫을 때의 페이드)
    pub fn place(&self, at: D2D_RECT_F, alpha: f32) -> Result<()> {
        let scale = Vector3 { X: (at.right - at.left) / self.size.0, Y: (at.bottom - at.top) / self.size.1, Z: 1.0 };
        for v in std::iter::once(&self.over).chain(&self.under) {
            v.SetOffset(Vector3 { X: at.left, Y: at.top, Z: 0.0 })?;
            v.SetScale(scale)?;
        }
        if (alpha - self.alpha.get()).abs() > 1.0 / 256.0 {
            self.alpha.set(alpha);
            self.props.InsertScalar(&HSTRING::from("alpha"), alpha)?;
        }
        Ok(())
    }

    /// 켜고 끔, 켜는 동안 계속 움직임
    pub fn set(&self, on: bool) -> Result<()> {
        match (on, self.state.get()) {
            (true, State::On) | (false, State::Off) | (false, State::Fading(_)) => Ok(()),
            // 지금 밝기에서 이어감 (사라지는 중에 다시 켜도 튀지 않게)
            (true, _) => {
                self.start()?;
                self.fade(&[(1.0, 1.0)], FADE_IN)?;
                self.state.set(State::On);
                Ok(())
            }
            (false, State::On) => {
                self.fade(&[(1.0, 0.0)], FADE_OUT)?;
                self.state.set(State::Fading(Instant::now()));
                Ok(())
            }
        }
    }

    /// 한 번 보여 주기: 나타나서 `hold`초 뒤 사라짐
    pub fn once(&self, hold: f32) -> Result<()> {
        self.start()?;
        let total = hold + FADE_OUT;
        self.fade(&[(0.0, 0.0), (FADE_IN.min(hold) / total, 1.0), (hold / total, 1.0), (1.0, 0.0)], total)?;
        self.state.set(State::Fading(Instant::now() + Duration::from_secs_f32(hold)));
        Ok(())
    }

    /// 다 사라진 빛의 움직임을 멈춤, 주기 타이머에서 부름
    /// 안 멈추면 안 보이는 빛도 합성기가 계속 돌림
    pub fn settle(&self) {
        if let State::Fading(at) = self.state.get() {
            if at.elapsed() >= Duration::from_secs_f32(FADE_OUT + 0.1) {
                self.stop();
            }
        }
    }

    /// 바로 숨기고 움직임을 멈춤 (창을 숨길 때)
    pub fn stop(&self) {
        if self.state.get() == State::Off {
            return;
        }
        self.state.set(State::Off);
        for (obj, name, _) in &self.motion {
            let _ = obj.StopAnimation(name);
        }
        let _ = self.props.StopAnimation(&HSTRING::from("fade"));
        let _ = self.props.InsertScalar(&HSTRING::from("fade"), 0.0);
        for v in std::iter::once(&self.over).chain(&self.under) {
            let _ = v.SetIsVisible(false);
        }
    }

    fn start(&self) -> Result<()> {
        if self.state.get() == State::Off {
            for (obj, name, anim) in &self.motion {
                obj.StartAnimation(name, anim)?;
            }
        }
        for v in std::iter::once(&self.over).chain(&self.under) {
            v.SetIsVisible(true)?;
        }
        Ok(())
    }

    /// 나타남 정도 (0..1)를 `keys` (진행, 값)대로, 구간마다 ease 곡선 (0.25, 0.1, 0.25, 1)
    /// 0에 키가 없으면 지금 값에서 시작
    fn fade(&self, keys: &[(f32, f32)], secs: f32) -> Result<()> {
        let c = &self.compositor;
        let a = c.CreateScalarKeyFrameAnimation()?;
        let ease = c.CreateCubicBezierEasingFunction(Vector2 { X: 0.25, Y: 0.1 }, Vector2 { X: 0.25, Y: 1.0 })?;
        for &(at, v) in keys {
            if at == 0.0 {
                a.InsertKeyFrame(at, v)?;
            } else {
                a.InsertKeyFrameWithEasingFunction(at, v, &ease)?;
            }
        }
        a.SetDuration(ts(secs))?;
        self.props.StartAnimation(&HSTRING::from("fade"), &a)
    }
}

/// 키프레임 사이 곡선: 직선, ease-in-out, 사인 in-out
#[derive(Clone, Copy)]
enum Curve {
    Linear,
    InOut,
    Sine,
}

fn ts(secs: f32) -> TimeSpan {
    TimeSpan { Duration: (secs * 10_000_000.0) as i64 }
}

/// 숫자를 식에 더하기·빼기로 붙임 (" + 2", " - 2")
fn plus(v: f32) -> String {
    if v >= 0.0 {
        format!(" + {v}")
    } else {
        format!(" - {}", -v)
    }
}

fn rgb(c: [f32; 3], a: f32) -> Color {
    let b = |v: f32| (v.clamp(0.0, 255.0)).round() as u8;
    Color { A: (a.clamp(0.0, 1.0) * 255.0).round() as u8, R: b(c[0]), G: b(c[1]), B: b(c[2]) }
}

/// 밝기, 채도 순으로 색에 미리 입힘 (표준 saturate 행렬, 단계마다 0..1로 자름)
fn tint(c: [f32; 3], bright: f32, sat: f32) -> [f32; 3] {
    let v = sat;
    let m = [
        [0.213 + 0.787 * v, 0.715 - 0.715 * v, 0.072 - 0.072 * v],
        [0.213 - 0.213 * v, 0.715 + 0.285 * v, 0.072 - 0.072 * v],
        [0.213 - 0.213 * v, 0.715 - 0.715 * v, 0.072 + 0.928 * v],
    ];
    let b = c.map(|x| (x / 255.0 * bright).min(1.0));
    m.map(|r| (r[0] * b[0] + r[1] * b[1] + r[2] * b[2]).clamp(0.0, 1.0) * 255.0)
}

/// 효과에 넘기는 속성 값
enum Prop {
    F32(f32),
    U32(u32),
}

/// 합성기가 프레임마다 적용하는 Direct2D 효과 (흐림, 색 회전)
/// 이름 붙인 속성 (`named`)은 합성기 애니메이션으로 움직임
#[implement(IGraphicsEffect, IGraphicsEffectSource, IGraphicsEffectD2D1Interop)]
struct Fx {
    name: RefCell<HSTRING>,
    id: GUID,
    props: Vec<Prop>,
    named: Vec<(&'static str, u32)>,
    sources: Vec<IGraphicsEffectSource>,
}

impl Fx {
    fn make(name: &str, id: GUID, props: Vec<Prop>, named: Vec<(&'static str, u32)>, sources: Vec<IGraphicsEffectSource>) -> IGraphicsEffect {
        Fx { name: RefCell::new(HSTRING::from(name)), id, props, named, sources }.into()
    }
}

impl IGraphicsEffect_Impl for Fx_Impl {
    fn Name(&self) -> Result<HSTRING> {
        Ok(self.name.borrow().clone())
    }
    fn SetName(&self, name: &HSTRING) -> Result<()> {
        *self.name.borrow_mut() = name.clone();
        Ok(())
    }
}

impl IGraphicsEffectSource_Impl for Fx_Impl {}

impl IGraphicsEffectD2D1Interop_Impl for Fx_Impl {
    fn GetEffectId(&self) -> Result<GUID> {
        Ok(self.id)
    }
    fn GetNamedPropertyMapping(&self, name: &PCWSTR, index: *mut u32, mapping: *mut GRAPHICS_EFFECT_PROPERTY_MAPPING) -> Result<()> {
        let name = unsafe { name.to_string() }.unwrap_or_default();
        let Some(&(_, i)) = self.named.iter().find(|(n, _)| *n == name) else { return Err(E_INVALIDARG.into()) };
        unsafe {
            *index = i;
            *mapping = GRAPHICS_EFFECT_PROPERTY_MAPPING_DIRECT;
        }
        Ok(())
    }
    fn GetPropertyCount(&self) -> Result<u32> {
        Ok(self.props.len() as u32)
    }
    fn GetProperty(&self, index: u32) -> Result<IPropertyValue> {
        match self.props.get(index as usize) {
            Some(Prop::F32(v)) => PropertyValue::CreateSingle(*v)?.cast(),
            Some(Prop::U32(v)) => PropertyValue::CreateUInt32(*v)?.cast(),
            None => Err(E_INVALIDARG.into()),
        }
    }
    fn GetSource(&self, index: u32) -> Result<IGraphicsEffectSource> {
        self.sources.get(index as usize).cloned().ok_or_else(|| E_INVALIDARG.into())
    }
    fn GetSourceCount(&self) -> Result<u32> {
        Ok(self.sources.len() as u32)
    }
}

/// 타원 그라데이션 조각: 멈춤점 (위치, 색), 가운데·반지름·불투명도는 속성 집합 `p`를 따라가는 식
struct Spot {
    stops: Vec<(f32, Color)>,
    center: String,
    radius: String,
    opacity: Option<String>,
}

/// 빛 조립용, `w`·`h`는 판 크기 (px), `s`는 DIP당 px
struct Make<'a> {
    c: &'a Compositor,
    graphics: &'a CompositionGraphicsDevice,
    props: &'a CompositionPropertySet,
    w: f32,
    h: f32,
    s: f32,
    motion: RefCell<Vec<(CompositionObject, HSTRING, CompositionAnimation)>>,
}

impl Make<'_> {
    fn sprite(&self, w: f32, h: f32, brush: &CompositionBrush) -> Result<SpriteVisual> {
        let v = self.c.CreateSpriteVisual()?;
        v.SetSize(Vector2 { X: w, Y: h })?;
        v.SetBrush(brush)?;
        Ok(v)
    }

    fn holder(&self, w: f32, h: f32) -> Result<ContainerVisual> {
        let v = self.c.CreateContainerVisual()?;
        v.SetSize(Vector2 { X: w, Y: h })?;
        Ok(v)
    }

    /// 비주얼 묶음을 한 장의 표면으로 (겹 마스크·효과를 한 번에 씌우려고)
    fn surface_of(&self, v: &ContainerVisual, w: f32, h: f32) -> Result<CompositionBrush> {
        let vs = self.c.CreateVisualSurface()?;
        vs.SetSourceVisual(v)?;
        vs.SetSourceSize(Vector2 { X: w, Y: h })?;
        self.c.CreateSurfaceBrushWithSurface(&vs)?.cast()
    }

    fn masked(&self, source: &CompositionBrush, mask: &CompositionBrush) -> Result<CompositionBrush> {
        let m = self.c.CreateMaskBrush()?;
        m.SetSource(source)?;
        m.SetMask(mask)?;
        m.cast()
    }

    /// 두 마스크를 곱한 마스크 (표면으로)
    fn both(&self, a: &CompositionBrush, b: &CompositionBrush) -> Result<CompositionBrush> {
        let hold = self.holder(self.w, self.h)?;
        hold.Children()?.InsertAtTop(&self.sprite(self.w, self.h, &self.masked(a, b)?)?)?;
        self.surface_of(&hold, self.w, self.h)
    }

    /// 한 번 그려 두는 모양 (px), Direct2D로 그림
    fn drawn(&self, w: f32, h: f32, paint: impl FnOnce(&ID2D1DeviceContext) -> Result<()>) -> Result<CompositionBrush> {
        let surface = self.graphics.CreateDrawingSurface(
            Size { Width: w.ceil(), Height: h.ceil() },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )?;
        unsafe {
            let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
            let mut at = POINT::default();
            let dc: ID2D1DeviceContext = interop.BeginDraw(None, &mut at)?;
            dc.SetTransform(&Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: at.x as f32, M32: at.y as f32 });
            dc.Clear(Some(&gfx::color(0.0, 0.0, 0.0, 0.0)));
            let r = paint(&dc);
            interop.EndDraw()?;
            r?;
        }
        // 표면은 정수 크기라 늘이지 않고 왼쪽 위에 맞춤
        let b = self.c.CreateSurfaceBrushWithSurface(&surface)?;
        b.SetStretch(CompositionStretch::None)?;
        b.SetHorizontalAlignmentRatio(0.0)?;
        b.SetVerticalAlignmentRatio(0.0)?;
        b.cast()
    }

    /// 속성 집합 `p`를 따라가는 식
    fn expr(&self, text: &str) -> Result<ExpressionAnimation> {
        let e = self.c.CreateExpressionAnimationWithExpression(&HSTRING::from(text))?;
        e.SetReferenceParameter(&HSTRING::from("p"), self.props)?;
        Ok(e)
    }

    fn bind(&self, obj: &CompositionObject, prop: &str, text: &str) -> Result<()> {
        obj.StartAnimation(&HSTRING::from(prop), &self.expr(text)?)
    }

    fn ease(&self, curve: Curve) -> Result<CompositionEasingFunction> {
        let bezier = |a: f32, b: f32| self.c.CreateCubicBezierEasingFunction(Vector2 { X: a, Y: 0.0 }, Vector2 { X: b, Y: 1.0 })?.cast();
        match curve {
            Curve::Linear => self.c.CreateLinearEasingFunction()?.cast(),
            Curve::InOut => bezier(0.42, 0.58),
            Curve::Sine => bezier(0.37, 0.63),
        }
    }

    /// 켤 때마다 다시 거는 반복 움직임 (속성 집합의 `name`을 `keys`대로, 구간마다 `curve`)
    fn motion(&self, name: &str, secs: f32, keys: &[(f32, f32)], curve: Curve, delay: f32) -> Result<()> {
        self.props.InsertScalar(&HSTRING::from(name), keys[0].1)?;
        let a = self.c.CreateScalarKeyFrameAnimation()?;
        let ease = self.ease(curve)?;
        for &(at, v) in keys {
            a.InsertKeyFrameWithEasingFunction(at, v, &ease)?;
        }
        a.SetDuration(ts(secs))?;
        a.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
        if delay > 0.0 {
            a.SetDelayTime(ts(delay))?;
            a.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
        }
        self.motion.borrow_mut().push((self.props.cast()?, HSTRING::from(name), a.cast()?));
        Ok(())
    }

    /// 조각들을 한 칸 (`w`×`h`)에 모아 표면으로, `extra`는 맨 위에 덮는 그림
    fn group(&self, spots: &[Spot], w: f32, h: f32, extra: Option<&CompositionBrush>) -> Result<ContainerVisual> {
        let hold = self.holder(w, h)?;
        for spot in spots {
            let b = self.c.CreateRadialGradientBrush()?;
            b.SetMappingMode(CompositionMappingMode::Absolute)?;
            for &(o, color) in &spot.stops {
                b.ColorStops()?.Append(&self.c.CreateColorGradientStopWithOffsetAndColor(o, color)?)?;
            }
            let bo: CompositionObject = b.cast()?;
            self.bind(&bo, "EllipseCenter", &spot.center)?;
            self.bind(&bo, "EllipseRadius", &spot.radius)?;
            let v = self.sprite(w, h, &b.cast()?)?;
            if let Some(o) = &spot.opacity {
                self.bind(&v.cast()?, "Opacity", o)?;
            }
            hold.Children()?.InsertAtTop(&v)?;
        }
        if let Some(e) = extra {
            hold.Children()?.InsertAtTop(&self.sprite(w, h, e)?)?;
        }
        Ok(hold)
    }

    /// 흐림 뒤 색 회전, `blur`는 흐림 표준편차 px (0이면 안 흐림), `hue`는 (폭 °, 주기 초)
    fn effect(&self, source: &CompositionBrush, blur: f32, hue: Option<(f32, f32)>) -> Result<CompositionBrush> {
        if blur <= 0.0 && hue.is_none() {
            return Ok(source.clone());
        }
        let mut src: IGraphicsEffectSource = CompositionEffectSourceParameter::Create(&HSTRING::from("source"))?.cast()?;
        if blur > 0.0 {
            src = Fx::make(
                "Blur",
                CLSID_D2D1GaussianBlur,
                vec![Prop::F32(blur), Prop::U32(D2D1_GAUSSIANBLUR_OPTIMIZATION_BALANCED.0 as u32), Prop::U32(D2D1_BORDER_MODE_SOFT.0 as u32)],
                vec![],
                vec![src],
            )
            .cast()?;
        }
        let brush = match hue {
            Some((range, period)) => {
                let fx = Fx::make("Hue", CLSID_D2D1HueRotation, vec![Prop::F32(-range)], vec![("Angle", 0)], vec![src]);
                let names = windows_collections::IIterable::<HSTRING>::from(vec![HSTRING::from("Hue.Angle")]);
                let b = self.c.CreateEffectFactoryWithProperties(&fx, &names)?.CreateBrush()?;
                let a = self.c.CreateScalarKeyFrameAnimation()?;
                let ease = self.ease(Curve::InOut)?;
                for (k, v) in [(0.0, -range), (0.5, range), (1.0, -range)] {
                    a.InsertKeyFrameWithEasingFunction(k, v, &ease)?;
                }
                a.SetDuration(ts(period))?;
                a.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
                self.motion.borrow_mut().push((b.cast()?, HSTRING::from("Hue.Angle"), a.cast()?));
                b
            }
            None => self.c.CreateEffectFactory(&src.cast::<IGraphicsEffect>()?)?.CreateBrush()?,
        };
        brush.SetSourceParameter(&HSTRING::from("source"), source)?;
        brush.cast()
    }

    /// 빛 자리를 따라가는 흰 타원 마스크, 알파는 가운데 1, `mid`에서 0.5, 끝 0
    fn spot_mask(&self, mid: f32, rx: &str, ry: &str) -> Result<CompositionBrush> {
        let b = self.c.CreateRadialGradientBrush()?;
        b.SetMappingMode(CompositionMappingMode::Absolute)?;
        for (o, a) in [(0.0, 1.0), (mid, 0.5), (1.0, 0.0)] {
            b.ColorStops()?.Append(&self.c.CreateColorGradientStopWithOffsetAndColor(o, rgb([255.0; 3], a))?)?;
        }
        let bo: CompositionObject = b.cast()?;
        self.bind(&bo, "EllipseCenter", &format!("Vector2(p.x * {}, {})", self.w, self.h))?;
        self.bind(&bo, "EllipseRadius", &format!("Vector2({rx} * {s}, {ry} * {s})", s = self.s))?;
        b.cast()
    }

    /// 판 테두리의 1 DIP 고리 (바깥 모서리 `outer`)
    fn ring(&self, outer: f32) -> Result<CompositionBrush> {
        let (w, h, s) = (self.w, self.h, self.s);
        self.drawn(w, h, |dc| unsafe {
            let f = dc.GetFactory()?;
            let a = f.CreateRoundedRectangleGeometry(&rounded(rect(0.0, 0.0, w, h), outer))?;
            let b = f.CreateRoundedRectangleGeometry(&rounded(rect(s, s, w - s, h - s), (outer - s).max(0.0)))?;
            let path = f.CreatePathGeometry()?;
            let sink = path.Open()?;
            a.CombineWithGeometry(&b, D2D1_COMBINE_MODE_EXCLUDE, None, 0.25, &sink)?;
            sink.Close()?;
            dc.FillGeometry(&path, &dc.CreateSolidColorBrush(&gfx::white(1.0), None)?, None);
            Ok(())
        })
    }

    /// 가장자리 띠: 위아래·좌우 28 DIP에서 흐려지는 두 띠를 더함
    fn edges(&self) -> Result<CompositionBrush> {
        let (w, h, s) = (self.w, self.h, self.s);
        self.drawn(w, h, |dc| unsafe {
            let e = 28.0 * s;
            let band = |len: f32| {
                let a = (e / len).min(1.0);
                let b = ((len - e) / len).max(a);
                [(0.0, 1.0), (a, 0.0), (b, 0.0), (1.0, 1.0)].map(|(position, al)| D2D1_GRADIENT_STOP { position, color: gfx::white(al) })
            };
            let full = rect(0.0, 0.0, w, h);
            dc.FillRectangle(&full, &linear(dc, &band(h), (0.0, 0.0), (0.0, h))?);
            dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_ADD);
            dc.FillRectangle(&full, &linear(dc, &band(w), (0.0, 0.0), (w, 0.0))?);
            dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_SOURCE_OVER);
            Ok(())
        })
    }

    /// 판 안쪽 그림자, 가장자리 1 DIP 안까지 흰색 `a`로 칠해 흐림 (표준편차 4.5 DIP)
    unsafe fn inset(&self, dc: &ID2D1DeviceContext, radius: f32, a: f32) -> Result<()> {
        let (w, h, s) = (self.w, self.h, self.s);
        let f = dc.GetFactory()?;
        let big = f.CreateRectangleGeometry(&rect(-40.0 * s, -40.0 * s, w + 40.0 * s, h + 40.0 * s))?;
        let hole = f.CreateRoundedRectangleGeometry(&rounded(rect(s, s, w - s, h - s), (radius - s).max(0.0)))?;
        let frame = f.CreatePathGeometry()?;
        let sink = frame.Open()?;
        big.CombineWithGeometry(&hole, D2D1_COMBINE_MODE_EXCLUDE, None, 0.25, &sink)?;
        sink.Close()?;
        let cl = dc.CreateCommandList()?;
        let home = dc.GetTarget()?;
        let mut place = Matrix3x2::identity();
        dc.GetTransform(&mut place);
        dc.SetTransform(&Matrix3x2::identity());
        dc.SetTarget(&cl);
        dc.FillGeometry(&frame, &dc.CreateSolidColorBrush(&gfx::white(a), None)?, None);
        cl.Close()?;
        dc.SetTarget(&home);
        dc.SetTransform(&place);
        let blur = dc.CreateEffect(&CLSID_D2D1GaussianBlur)?;
        gfx::set_f32(&blur, D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32, 4.5 * s)?;
        gfx::set_input_image(&blur, 0, &cl.cast()?);
        let body: ID2D1Geometry = f.CreateRoundedRectangleGeometry(&rounded(rect(0.0, 0.0, w, h), radius))?.cast()?;
        dc.PushLayer(&clip_layer(&body), None);
        dc.DrawImage(&blur.GetOutput()?, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
        dc.PopLayer();
        Ok(())
    }

    /// 조각 그라데이션을 Direct2D로 한 번 그림 (움직이지 않는 조각)
    unsafe fn paint_spot(&self, dc: &ID2D1DeviceContext, c: [f32; 3], a: f32, center: (f32, f32), radius: (f32, f32)) -> Result<()> {
        let col = |al: f32| D2D1_COLOR_F { r: c[0] / 255.0, g: c[1] / 255.0, b: c[2] / 255.0, a: al };
        let stops = [D2D1_GRADIENT_STOP { position: 0.0, color: col(a) }, D2D1_GRADIENT_STOP { position: 1.0, color: col(0.0) }];
        let rt: ID2D1RenderTarget = dc.cast()?;
        let coll = rt.CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
        let b = dc.CreateRadialGradientBrush(
            &D2D1_RADIAL_GRADIENT_BRUSH_PROPERTIES { center: gfx::pt(center.0, center.1), gradientOriginOffset: gfx::pt(0.0, 0.0), radiusX: radius.0, radiusY: radius.1 },
            None,
            &coll,
        )?;
        dc.FillRectangle(&rect(0.0, 0.0, self.w, self.h), &b);
        Ok(())
    }

    /// 판 모양으로 자름
    fn clip(&self, v: &ContainerVisual, w: f32, h: f32, radius: f32) -> Result<()> {
        let g = self.c.CreateRoundedRectangleGeometry()?;
        g.SetSize(Vector2 { X: w, Y: h })?;
        g.SetCornerRadius(Vector2 { X: radius, Y: radius })?;
        v.SetClip(&self.c.CreateGeometricClipWithGeometry(&g)?)
    }

    /// 겹 하나를 `parent`에 붙임, 모은 빛을 마스크로 가림
    /// 불투명도는 나타남 × 판 불투명도 × `k`, `edge`면 양 끝에서 흐려지는 값도 곱함
    fn layer(&self, parent: &ContainerVisual, source: &CompositionBrush, mask: &CompositionBrush, k: f32, edge: bool) -> Result<()> {
        let v = self.sprite(self.w, self.h, &self.masked(source, mask)?)?;
        let e = if edge { " * p.edge" } else { "" };
        self.bind(&v.cast()?, "Opacity", &format!("Clamp(p.fade * p.alpha{e} * {k}, 0, 1)"))?;
        parent.Children()?.InsertAtTop(&v)
    }

    // ---------------------------------------------------------------- 도는 빛

    /// 원뿔 그라데이션 무늬 셋 (가림, 흰 심, 번짐)을 판 대각선보다 큰 정사각형에 그려 두고 합성기가 돌림
    /// 움직이지 않는 회색 조각과 안쪽 빛은 Direct2D로 한 번 그림
    fn spin(&self, root: &ContainerVisual, radius: f32) -> Result<()> {
        let (w, h, s) = (self.w, self.h, self.s);
        self.motion("angle", SPIN_SECS, &[(0.0, 0.0), (1.0, 360.0)], Curve::Linear, 0.0)?;
        let side = ((w * w + h * h).sqrt().ceil() as u32 + 2) & !1;
        let [mask, core, bloom] = conic_images(side);
        let turning = |pixels: Vec<u8>, blur: f32| -> Result<ContainerVisual> {
            let l = side as f32;
            let img = self.drawn(l, l, |dc| unsafe {
                let props = D2D1_BITMAP_PROPERTIES1 {
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                    colorContext: std::mem::ManuallyDrop::new(None),
                };
                let bmp = dc.CreateBitmap(D2D_SIZE_U { width: side, height: side }, Some(pixels.as_ptr() as _), side * 4, &props)?;
                if blur > 0.0 {
                    let fx = dc.CreateEffect(&CLSID_D2D1GaussianBlur)?;
                    gfx::set_f32(&fx, D2D1_GAUSSIANBLUR_PROP_STANDARD_DEVIATION.0 as u32, blur)?;
                    gfx::set_input_image(&fx, 0, &bmp.cast()?);
                    dc.DrawImage(&fx.GetOutput()?, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
                } else {
                    dc.DrawBitmap(&bmp, None, 1.0, D2D1_INTERPOLATION_MODE_NEAREST_NEIGHBOR, None, None);
                }
                Ok(())
            })?;
            let v = self.sprite(l, l, &img)?;
            v.SetOffset(Vector3 { X: (w - l) / 2.0, Y: (h - l) / 2.0, Z: 0.0 })?;
            v.SetCenterPoint(Vector3 { X: l / 2.0, Y: l / 2.0, Z: 0.0 })?;
            self.bind(&v.cast()?, "RotationAngleInDegrees", "p.angle")?;
            let hold = self.holder(w, h)?;
            hold.Children()?.InsertAtTop(&v)?;
            Ok(hold)
        };
        let (mask_v, core_v, bloom_v) = (turning(mask, 0.0)?, turning(core, 0.0)?, turning(bloom, 8.0 * s)?);
        let mask_b = self.surface_of(&mask_v, w, h)?;
        let ring = self.ring(radius - s)?;
        // 테두리 선: 회색 조각 위에 흰 심, 도는 가림 × 고리
        let greys = self.drawn(w, h, |dc| unsafe {
            for &(c, x, y, rx, ry) in SPIN_GREYS.iter().rev() {
                self.paint_spot(dc, [c; 3], 1.0, (x * w, y * h), (rx * s, ry * s))?;
            }
            Ok(())
        })?;
        let stroke_v = self.holder(w, h)?;
        stroke_v.Children()?.InsertAtTop(&self.sprite(w, h, &greys)?)?;
        stroke_v.Children()?.InsertAtTop(&core_v)?;
        // 안쪽 빛: 옅은 회색 조각과 안쪽 그림자, 도는 가림 × 가장자리 띠
        let inner = self.drawn(w, h, |dc| unsafe {
            for &(c, x, y, rx, ry) in SPIN_GREYS.iter().rev() {
                self.paint_spot(dc, [c; 3], 0.225, (x * w, y * h), ((rx * 0.9).round() * s, (ry * 0.9).round() * s))?;
            }
            self.inset(dc, radius, 0.27)
        })?;
        self.clip(root, w, h, radius)?;
        self.layer(root, &inner, &self.both(&mask_b, &self.edges()?)?, 0.21, false)?;
        self.layer(root, &self.surface_of(&stroke_v, w, h)?, &self.both(&mask_b, &ring)?, 0.13, false)?;
        self.layer(root, &self.surface_of(&bloom_v, w, h)?, &ring, 0.12, false)
    }

    // ---------------------------------------------------------------- 아래 테두리를 오가는 빛

    fn line(&self, root: &ContainerVisual, radius: f32, colorful: bool) -> Result<()> {
        let (w, h, s) = (self.w, self.h, self.s);
        let p10 = |v: [f32; 11]| -> Vec<(f32, f32)> { v.iter().enumerate().map(|(i, &x)| (i as f32 / 10.0, x)).collect() };
        self.motion("x", 3.1, &p10([0.06, 0.15, 0.25, 0.35, 0.44, 0.5, 0.56, 0.65, 0.75, 0.85, 0.94]), Curve::Linear, 0.0)?;
        self.motion("w", 3.1, &p10([0.5, 0.8, 1.1, 1.3, 1.45, 1.5, 1.45, 1.3, 1.1, 0.8, 0.5]), Curve::Linear, 0.0)?;
        self.motion("edge", 3.1, &[(0.0, 0.0), (0.125, 0.0), (0.325, 1.0), (0.675, 1.0), (0.875, 0.0), (1.0, 0.0)], Curve::Linear, 0.0)?;
        self.motion("h", 4.0, &[(0.0, 0.8), (0.25, 1.25), (0.55, 0.85), (0.8, 1.3), (1.0, 0.8)], Curve::InOut, 0.0)?;
        self.motion("spike", 4.1, &[(0.0, 0.8), (0.25, 1.3), (0.5, 0.9), (0.75, 1.4), (1.0, 0.8)], Curve::InOut, 0.0)?;
        self.motion("spike2", 5.3, &[(0.0, 1.2), (0.25, 0.7), (0.5, 1.4), (0.75, 0.8), (1.0, 1.2)], Curve::InOut, 0.0)?;
        let (stroke, inner, bloom) = if colorful { (&LINE_STROKE, &LINE_INNER, &LINE_BLOOM) } else { (&LINE_STROKE_MONO, &LINE_INNER_MONO, &LINE_BLOOM_MONO) };
        // 여러 색만 밝기 1.3배, 채도와 흰색은 그대로
        let col = |c: [f32; 3], a: f32| if colorful { rgb(tint(c, 1.3, 1.0), a) } else { rgb(c, a) };
        let at = |x: f32, abs: bool, dy: f32| {
            let x = if abs { format!("{x} * {w}") } else { format!("p.x * {w}{}", plus(x * s)) };
            format!("Vector2({x}, {h}{})", plus(dy * s))
        };
        let spots = |list: &[LineSpot]| -> Vec<Spot> {
            list.iter()
                .rev()
                .map(|b| Spot {
                    stops: b.stops.iter().map(|&(o, c, a)| (o, col(c, a))).collect(),
                    center: at(b.x, b.fixed, b.dy),
                    radius: format!("Vector2({} * {} * {s}, {} * {} * {s})", b.rx, b.wk, b.ry, b.hk),
                    opacity: None,
                })
                .collect()
        };
        let shade = self.drawn(w, h, |dc| unsafe { self.inset(dc, radius, if colorful { 0.13 } else { 0.1 }) })?;
        let hue = |range, period| colorful.then_some((range, period));
        let inner_src = self.effect(&self.surface_of(&self.group(&spots(inner), w, h, Some(&shade))?, w, h)?, 0.0, hue(13.0, 12.0))?;
        let stroke_src = self.effect(&self.surface_of(&self.group(&spots(stroke), w, h, None)?, w, h)?, 0.0, hue(13.0, 12.0))?;
        let blur = (if colorful { 8.0 } else { 6.0 }) * s;
        let bloom_src = self.effect(&self.surface_of(&self.group(&spots(bloom), w, h, None)?, w, h)?, blur, hue(23.0, 8.0))?;
        let mask_of = |shape: &CompositionBrush| self.both(&self.spot_mask(0.45, "78 * p.w", "60 * p.h")?, shape);
        self.clip(root, w, h, radius)?;
        self.layer(root, &inner_src, &mask_of(&self.edges()?)?, 0.7, true)?;
        self.layer(root, &stroke_src, &mask_of(&self.ring(radius - s)?)?, 1.14, true)?;
        self.layer(root, &bloom_src, &self.spot_mask(0.35, "84 * p.w", "110 * p.h")?, 0.8, true)
    }

    // ---------------------------------------------------------------- 판 바깥으로 번지는 빛

    /// 판 테두리 빛 (위 층)과 판 뒤로 번지는 빛 둘 (아래 층, 10 · 30 DIP 넓힌 칸을 흐림)
    /// 조각마다 크기·자리·밝기가 서로 다른 주기로 오감
    fn glow(&self, over: &ContainerVisual, under: &ContainerVisual, radius: f32, failed: bool) -> Result<()> {
        let (w, h, s) = (self.w, self.h, self.s);
        let palette = if failed { &GLOW_FAILED } else { &GLOW_DONE };
        let col = |ci: usize, a: f32| rgb(tint(palette[ci], 1.9, 1.2), a);
        // 판 크기에 맞춰 빛을 늘림, 수치는 350 × 140 DIP 판 기준이고 0.35~4배까지
        let (sx, sy) = ((w / s / 350.0).clamp(0.35, 4.0), (h / s / 140.0).clamp(0.35, 4.0));
        // 오가는 값 (이름, 처음, 반대쪽, 주기 초, 늦춤 초)
        // 폭은 sp 크기, dr 자리 (DIP), op 밝기, gh 높이, 기준 주기는 bs 자리·밝기, ss 크기, ghs 높이
        let (sp, dr, op, gh, bs, ss, ghs) = (0.28f32, 14.0f32, 0.46f32, 0.16f32, 2.3f32, 6.4f32, 2.4f32);
        let waves: [(&str, f32, f32, f32, f32); 17] = [
            ("bw1", 1.0 - sp, 1.0 + sp * 1.1, ss * 0.9, 0.0),
            ("bh1", 1.0 + sp * 0.9, 1.0 - sp * 0.85, ss * 1.26, 0.0),
            ("bx1", -dr, dr * 0.9, bs * 1.6, 0.0),
            ("by1", dr * 0.55, -dr * 0.7, bs * 1.6, 0.0),
            ("bw2", 1.0 + sp, 1.0 - sp * 0.85, ss * 1.1, 0.0),
            ("bh2", 1.0 - sp * 0.8, 1.0 + sp * 1.05, ss * 0.81, 0.0),
            ("bx2", dr * 0.8, -dr * 0.9, bs * 1.88, 0.0),
            ("by2", -dr, dr * 0.65, bs * 1.88, 0.0),
            ("bw3", 1.0 - sp * 0.6, 1.0 + sp * 1.15, ss * 0.98, 0.0),
            ("bh3", 1.0 + sp * 0.75, 1.0 - sp, ss * 1.4, 0.0),
            ("bx3", -dr * 0.6, dr, bs * 1.45, 0.0),
            ("by3", -dr * 0.85, dr * 0.45, bs * 1.45, 0.0),
            ("bgh", 1.0 - gh, 1.0 + gh, ghs, 0.0),
            ("tl", 1.0 - op, 1.0, bs, 0.0),
            ("tr", 1.0 - op, 1.0, bs * 1.32, bs * 0.28),
            ("bl", 1.0 - op, 1.0, bs * 0.84, bs * 0.55),
            ("br", 1.0 - op, 1.0, bs * 1.58, bs * 0.83),
        ];
        for (name, a, b, period, delay) in waves {
            // 처음과 반대쪽 사이를 사인 곡선으로 오감
            self.motion(name, period, &[(0.0, a), (0.5, b), (1.0, a)], Curve::Sine, delay)?;
        }
        // 테두리 둘레 여덟 조각, 칸 (`bw`×`bh`)의 비율 자리에서 오가는 값만큼 움직임
        let edge_spots = |bw: f32, bh: f32| -> Vec<Spot> {
            GLOW_EDGE
                .iter()
                .rev()
                .map(|&(ci, region, quad, rx, ry, fx, fy)| Spot {
                    stops: vec![(0.0, col(ci, 1.0)), (1.0, col(ci, 0.0))],
                    center: format!("Vector2({fx} * {bw} + p.bx{region} * {s}, {fy} * {bh} + p.by{region} * {s})"),
                    radius: format!("Vector2({rx} * p.bw{region} * {sx} * {s}, {ry} * p.bh{region} * p.bgh * {sy} * {s})"),
                    opacity: Some(format!("p.{quad}")),
                })
                .collect()
        };
        // 위 층: 테두리 고리
        self.clip(over, w, h, radius)?;
        let edge_src = self.surface_of(&self.group(&edge_spots(w, h), w, h, None)?, w, h)?;
        self.layer(over, &edge_src, &self.ring(radius)?, 0.94, false)?;
        // 아래 층: `pad` DIP 넓힌 칸을 흐리고 가로 0.95, 세로 0.9로 줄임, `room`은 흐림이 퍼질 여백
        let behind = |pad: f32, blur: f32, k: f32, spots: Vec<Spot>| -> Result<()> {
            let (bw, bh, room) = (w + 2.0 * pad * s, h + 2.0 * pad * s, (2.5 * blur * s).ceil());
            let inside = self.group(&spots, bw, bh, None)?;
            self.clip(&inside, bw, bh, radius + pad * s)?;
            let frame = self.holder(bw + 2.0 * room, bh + 2.0 * room)?;
            inside.SetOffset(Vector3 { X: room, Y: room, Z: 0.0 })?;
            frame.Children()?.InsertAtTop(&inside)?;
            let (fw, fh) = (bw + 2.0 * room, bh + 2.0 * room);
            let v = self.sprite(fw, fh, &self.effect(&self.surface_of(&frame, fw, fh)?, blur * s, None)?)?;
            v.SetOffset(Vector3 { X: -pad * s - room, Y: -pad * s - room, Z: 0.0 })?;
            v.SetCenterPoint(Vector3 { X: fw / 2.0, Y: fh / 2.0, Z: 0.0 })?;
            v.SetScale(Vector3 { X: 0.95, Y: 0.9, Z: 1.0 })?;
            self.bind(&v.cast()?, "Opacity", &format!("Clamp(p.fade * p.alpha * {k}, 0, 1)"))?;
            under.Children()?.InsertAtTop(&v)
        };
        let (bw, bh) = (w + 60.0 * s, h + 60.0 * s);
        let bloom_spots = GLOW_BLOOM
            .iter()
            .rev()
            .map(|&(ci, rx, ry, fx, fy)| Spot {
                stops: vec![(0.0, col(ci, 1.0 - op * 0.5)), (1.0, col(ci, 0.0))],
                center: format!("Vector2({} , {})", fx * bw, fy * bh),
                radius: format!("Vector2({}, {})", rx * sx * s, ry * sy * s),
                opacity: None,
            })
            .collect();
        behind(30.0, 22.5, 0.3, bloom_spots)?;
        behind(10.0, 3.0, 0.34, edge_spots(w + 20.0 * s, h + 20.0 * s))
    }
}

/// 모양 마스크만 있는 레이어 (빌려 씀, 해제는 호출하는 쪽)
fn clip_layer(geo: &ID2D1Geometry) -> D2D1_LAYER_PARAMETERS1 {
    unsafe {
        D2D1_LAYER_PARAMETERS1 {
            contentBounds: rect(-1e6, -1e6, 1e6, 1e6),
            geometricMask: std::mem::ManuallyDrop::new(Some(std::mem::transmute_copy(geo))),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: Matrix3x2::identity(),
            opacity: 1.0,
            opacityBrush: std::mem::ManuallyDrop::new(None),
            layerOptions: D2D1_LAYER_OPTIONS1_NONE,
        }
    }
}

unsafe fn linear(dc: &ID2D1DeviceContext, stops: &[D2D1_GRADIENT_STOP], from: (f32, f32), to: (f32, f32)) -> Result<ID2D1LinearGradientBrush> {
    let rt: ID2D1RenderTarget = dc.cast()?;
    let coll = rt.CreateGradientStopCollection(stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)?;
    dc.CreateLinearGradientBrush(&D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES { startPoint: gfx::pt(from.0, from.1), endPoint: gfx::pt(to.0, to.1) }, None, &coll)
}

/// 원뿔 그라데이션 셋 (가림, 흰 심, 번짐), 12시부터 시계 방향 비율에 따른 흰색 알파, premultiplied BGRA
fn conic_images(side: u32) -> [Vec<u8>; 3] {
    let n = (side * side * 4) as usize;
    let mut out = [vec![0u8; n], vec![0u8; n], vec![0u8; n]];
    let c = side as f32 / 2.0;
    for y in 0..side {
        for x in 0..side {
            let (dx, dy) = (x as f32 + 0.5 - c, y as f32 + 0.5 - c);
            let f = (dx.atan2(-dy) / std::f32::consts::TAU).rem_euclid(1.0);
            let i = ((y * side + x) * 4) as usize;
            for (img, stops) in out.iter_mut().zip([&SPIN_MASK[..], &SPIN_CORE[..], &SPIN_BLOOM[..]]) {
                let a = (stop_at(stops, f) * 255.0).round() as u8;
                img[i..i + 4].copy_from_slice(&[a, a, a, a]);
            }
        }
    }
    out
}

/// 멈춤점 (비율, 알파) 사이를 직선으로 이음
fn stop_at(stops: &[(f32, f32)], f: f32) -> f32 {
    for k in stops.windows(2) {
        if f <= k[1].0 {
            let t = if k[1].0 > k[0].0 { (f - k[0].0) / (k[1].0 - k[0].0) } else { 1.0 };
            return k[0].1 + (k[1].1 - k[0].1) * t;
        }
    }
    stops[stops.len() - 1].1
}

// ---------------------------------------------------------------- 빛 모양 수치 (DIP)

/// 도는 빛의 원뿔 무늬 (가림, 흰 심, 번짐), 멈춤점은 (12시부터 시계 방향 비율, 알파)
const SPIN_MASK: [(f32, f32); 10] = [(0.0, 0.0), (0.30, 0.0), (0.36, 0.1), (0.44, 0.35), (0.52, 1.0), (0.80, 1.0), (0.86, 0.35), (0.92, 0.1), (0.95, 0.0), (1.0, 0.0)];
const SPIN_CORE: [(f32, f32); 11] = [(0.0, 0.0), (0.54, 0.0), (0.57, 0.1), (0.60, 0.3), (0.63, 0.6), (0.66, 0.75), (0.69, 0.6), (0.72, 0.3), (0.75, 0.1), (0.78, 0.0), (1.0, 0.0)];
const SPIN_BLOOM: [(f32, f32); 14] = [
    (0.0, 0.0),
    (0.58, 0.0),
    (0.62, 0.03),
    (0.65, 0.08),
    (0.67, 0.2),
    (0.69, 0.45),
    (0.70, 0.85),
    (0.705, 0.85),
    (0.715, 0.45),
    (0.73, 0.2),
    (0.75, 0.08),
    (0.78, 0.03),
    (0.82, 0.0),
    (1.0, 0.0),
];
/// 도는 빛 테두리의 회색 조각 (회색 0..255, 자리 가로·세로 비율, 반지름 가로·세로 DIP), 앞에 있을수록 위
const SPIN_GREYS: [(f32, f32, f32, f32, f32); 9] = [
    (180.0, 0.33, -0.074, 70.0, 40.0),
    (140.0, 0.12, -0.05, 60.0, 35.0),
    (160.0, 0.021, 0.683, 40.0, 70.0),
    (130.0, 0.021, 0.683, 20.0, 35.0),
    (170.0, 0.744, 1.0, 180.0, 32.0),
    (150.0, 0.55, 1.0, 85.0, 26.0),
    (190.0, 0.939, 0.0, 74.0, 32.0),
    (145.0, 1.0, 0.271, 26.0, 42.0),
    (165.0, 1.0, 0.271, 52.0, 48.0),
];

/// 오가는 빛의 조각: 멈춤점 (위치, 색, 알파), 반지름 DIP와 거기 곱할 값 (`wk`, `hk`)
/// `x`는 빛 자리에서 DIP (`fixed`면 줄 너비 비율), `dy`는 아래 끝에서 DIP (아래로 +)
struct LineSpot {
    stops: &'static [(f32, [f32; 3], f32)],
    rx: f32,
    ry: f32,
    wk: &'static str,
    hk: &'static str,
    x: f32,
    fixed: bool,
    dy: f32,
}

#[allow(clippy::too_many_arguments)]
const fn spot(stops: &'static [(f32, [f32; 3], f32)], rx: f32, ry: f32, wk: &'static str, hk: &'static str, x: f32, fixed: bool, dy: f32) -> LineSpot {
    LineSpot { stops, rx, ry, wk, hk, x, fixed, dy }
}

macro_rules! fade_to {
    ($c:expr, $a:expr) => {
        &[(0.0, $c, $a), (1.0, $c, 0.0)]
    };
}

const W: [f32; 3] = [255.0, 255.0, 255.0];
/// 테두리 선: 빛 자리 둘레 아홉 색 + 흰 심 (앞에 있을수록 위)
const LINE_STROKE: [LineSpot; 10] = [
    spot(&[(0.0, W, 0.38), (0.30, W, 0.12), (0.65, W, 0.0)], 24.0, 28.0, "p.w", "p.h", 0.0, false, 2.0),
    spot(fade_to!([255.0, 50.0, 100.0], 1.0), 36.0, 36.0, "p.w", "p.h", 0.0, false, 2.0),
    spot(fade_to!([40.0, 180.0, 220.0], 1.0), 30.0, 32.0, "p.w", "p.h", 39.0, false, 0.0),
    spot(fade_to!([50.0, 200.0, 80.0], 1.0), 33.0, 28.0, "p.w", "p.h", -36.0, false, 2.0),
    spot(fade_to!([180.0, 40.0, 240.0], 1.0), 29.0, 34.0, "p.w", "p.h", -54.0, false, 0.0),
    spot(fade_to!([255.0, 160.0, 30.0], 1.0), 27.0, 30.0, "p.w", "p.h", 51.0, false, -1.0),
    spot(fade_to!([100.0, 70.0, 255.0], 1.0), 36.0, 24.0, "p.w", "p.h", 21.0, false, 1.0),
    spot(fade_to!([40.0, 140.0, 255.0], 1.0), 30.0, 22.0, "p.w", "p.h", -21.0, false, 0.0),
    spot(fade_to!([240.0, 50.0, 180.0], 1.0), 25.0, 28.0, "p.w", "p.h", 66.0, false, 1.0),
    spot(fade_to!([30.0, 185.0, 170.0], 1.0), 23.0, 30.0, "p.w", "p.h", -66.0, false, -1.0),
];
const LINE_STROKE_MONO: [LineSpot; 10] = [
    spot(&[(0.0, W, 0.38), (0.30, W, 0.12), (0.65, W, 0.0)], 24.0, 28.0, "p.w", "p.h", 0.0, false, 2.0),
    spot(fade_to!([200.0; 3], 1.0), 36.0, 36.0, "p.w", "p.h", 0.0, false, 2.0),
    spot(fade_to!([170.0; 3], 1.0), 30.0, 32.0, "p.w", "p.h", 39.0, false, 0.0),
    spot(fade_to!([155.0; 3], 1.0), 33.0, 28.0, "p.w", "p.h", -36.0, false, 2.0),
    spot(fade_to!([185.0; 3], 1.0), 29.0, 34.0, "p.w", "p.h", -54.0, false, 0.0),
    spot(fade_to!([165.0; 3], 1.0), 27.0, 30.0, "p.w", "p.h", 51.0, false, -1.0),
    spot(fade_to!([180.0; 3], 1.0), 36.0, 24.0, "p.w", "p.h", 21.0, false, 1.0),
    spot(fade_to!([160.0; 3], 1.0), 30.0, 22.0, "p.w", "p.h", -21.0, false, 0.0),
    spot(fade_to!([175.0; 3], 1.0), 25.0, 28.0, "p.w", "p.h", 66.0, false, 1.0),
    spot(fade_to!([190.0; 3], 1.0), 23.0, 30.0, "p.w", "p.h", -66.0, false, -1.0),
];
/// 안쪽 빛: 같은 자리에 조금 작고 옅게, 아래 끝에서 위로
const LINE_INNER: [LineSpot; 9] = [
    spot(fade_to!([255.0, 50.0, 100.0], 0.48), 33.0, 30.0, "p.w", "p.h", 0.0, false, 0.0),
    spot(fade_to!([40.0, 180.0, 220.0], 0.42), 24.0, 26.0, "p.w", "p.h", 39.0, false, -3.0),
    spot(fade_to!([50.0, 200.0, 80.0], 0.48), 27.0, 24.0, "p.w", "p.h", -36.0, false, 0.0),
    spot(fade_to!([180.0, 40.0, 240.0], 0.42), 23.0, 28.0, "p.w", "p.h", -54.0, false, -2.0),
    spot(fade_to!([255.0, 160.0, 30.0], 0.50), 24.0, 24.0, "p.w", "p.h", 51.0, false, -1.0),
    spot(fade_to!([100.0, 70.0, 255.0], 0.45), 30.0, 20.0, "p.w", "p.h", 21.0, false, 0.0),
    spot(fade_to!([40.0, 140.0, 255.0], 0.40), 25.0, 18.0, "p.w", "p.h", -21.0, false, -2.0),
    spot(fade_to!([240.0, 50.0, 180.0], 0.45), 21.0, 24.0, "p.w", "p.h", 66.0, false, 0.0),
    spot(fade_to!([30.0, 185.0, 170.0], 0.52), 18.0, 26.0, "p.w", "p.h", -66.0, false, -1.0),
];
const LINE_INNER_MONO: [LineSpot; 9] = [
    spot(fade_to!([200.0; 3], 0.48), 33.0, 30.0, "p.w", "p.h", 0.0, false, 0.0),
    spot(fade_to!([170.0; 3], 0.42), 24.0, 26.0, "p.w", "p.h", 39.0, false, -3.0),
    spot(fade_to!([155.0; 3], 0.48), 27.0, 24.0, "p.w", "p.h", -36.0, false, 0.0),
    spot(fade_to!([185.0; 3], 0.42), 23.0, 28.0, "p.w", "p.h", -54.0, false, -2.0),
    spot(fade_to!([165.0; 3], 0.50), 24.0, 24.0, "p.w", "p.h", 51.0, false, -1.0),
    spot(fade_to!([180.0; 3], 0.45), 30.0, 20.0, "p.w", "p.h", 21.0, false, 0.0),
    spot(fade_to!([160.0; 3], 0.40), 25.0, 18.0, "p.w", "p.h", -21.0, false, -2.0),
    spot(fade_to!([175.0; 3], 0.45), 21.0, 24.0, "p.w", "p.h", 66.0, false, 0.0),
    spot(fade_to!([190.0; 3], 0.52), 18.0, 26.0, "p.w", "p.h", -66.0, false, -1.0),
];
/// 번짐: 줄 너비에 고정된 가는 가시 일곱 개 + 빛 자리의 흰 심 둘
const LINE_BLOOM: [LineSpot; 9] = [
    spot(&[(0.0, [255.0, 60.0, 80.0], 1.0), (0.30, [255.0, 60.0, 80.0], 1.0), (0.88, [255.0, 60.0, 80.0], 0.0)], 0.8, 92.0, "p.spike", "p.h", 0.08, true, -2.0),
    spot(&[(0.0, [40.0, 190.0, 180.0], 0.98), (0.5, [40.0, 190.0, 180.0], 0.49), (0.95, [40.0, 190.0, 180.0], 0.0)], 10.0, 35.0, "p.spike2", "p.h", 0.22, true, -4.0),
    spot(&[(0.0, [100.0, 70.0, 255.0], 1.0), (0.40, [100.0, 70.0, 255.0], 1.0), (0.90, [100.0, 70.0, 255.0], 0.0)], 2.0, 72.0, "(2 - p.spike)", "p.h", 0.36, true, -3.0),
    spot(&[(0.0, [255.0, 170.0, 40.0], 0.59), (0.55, [255.0, 170.0, 40.0], 0.29), (0.96, [255.0, 170.0, 40.0], 0.0)], 14.0, 28.0, "p.spike2", "p.h", 0.50, true, -2.0),
    spot(&[(0.0, [50.0, 200.0, 100.0], 1.0), (0.35, [50.0, 200.0, 100.0], 1.0), (0.89, [50.0, 200.0, 100.0], 0.0)], 1.2, 85.0, "(2 - p.spike2)", "p.h", 0.64, true, -4.0),
    spot(&[(0.0, [200.0, 50.0, 240.0], 0.91), (0.48, [200.0, 50.0, 240.0], 0.45), (0.94, [200.0, 50.0, 240.0], 0.0)], 7.0, 45.0, "p.spike", "p.h", 0.78, true, -2.0),
    spot(&[(0.0, [40.0, 140.0, 255.0], 1.0), (0.42, [40.0, 140.0, 255.0], 1.0), (0.91, [40.0, 140.0, 255.0], 0.0)], 0.6, 60.0, "(2 - p.spike)", "p.h", 0.92, true, -3.0),
    spot(&[(0.0, W, 1.0), (0.20, W, 0.9), (0.50, W, 0.5), (1.0, W, 0.0)], 21.0, 15.0, "p.spike", "p.spike2", 0.0, false, 1.0),
    spot(&[(0.0, W, 0.3), (0.25, W, 0.12), (0.55, W, 0.03), (0.80, W, 0.0)], 42.0, 40.0, "p.w", "p.h", 0.0, false, 0.0),
];
const LINE_BLOOM_MONO: [LineSpot; 9] = [
    spot(&[(0.0, [200.0; 3], 0.14), (0.30, [200.0; 3], 0.09), (0.88, [200.0; 3], 0.0)], 12.0, 42.0, "p.spike", "p.h", 0.08, true, -2.0),
    spot(&[(0.0, [170.0; 3], 0.12), (0.5, [170.0; 3], 0.06), (0.95, [170.0; 3], 0.0)], 10.0, 35.0, "p.spike2", "p.h", 0.22, true, -4.0),
    spot(&[(0.0, [200.0; 3], 0.14), (0.40, [200.0; 3], 0.10), (0.90, [200.0; 3], 0.0)], 14.0, 38.0, "(2 - p.spike)", "p.h", 0.36, true, -3.0),
    spot(&[(0.0, [180.0; 3], 0.08), (0.55, [180.0; 3], 0.03), (0.96, [180.0; 3], 0.0)], 14.0, 28.0, "p.spike2", "p.h", 0.50, true, -2.0),
    spot(&[(0.0, [190.0; 3], 0.14), (0.35, [190.0; 3], 0.10), (0.89, [190.0; 3], 0.0)], 12.0, 40.0, "(2 - p.spike2)", "p.h", 0.64, true, -4.0),
    spot(&[(0.0, [170.0; 3], 0.13), (0.48, [170.0; 3], 0.04), (0.94, [170.0; 3], 0.0)], 7.0, 45.0, "p.spike", "p.h", 0.78, true, -2.0),
    spot(&[(0.0, [185.0; 3], 0.14), (0.42, [185.0; 3], 0.10), (0.91, [185.0; 3], 0.0)], 10.0, 32.0, "(2 - p.spike)", "p.h", 0.92, true, -3.0),
    spot(&[(0.0, W, 0.5), (0.20, W, 0.45), (0.50, W, 0.25), (1.0, W, 0.0)], 21.0, 15.0, "p.spike", "p.spike2", 0.0, false, 1.0),
    spot(&[(0.0, W, 0.15), (0.25, W, 0.06), (0.55, W, 0.015), (0.80, W, 0.0)], 42.0, 40.0, "p.w", "p.h", 0.0, false, 0.0),
];

/// 번지는 빛의 색, 완료 배지 초록 #13A065와 실패 배지 빨강 #F04452 둘레, 밝기 1.9·채도 1.2를 입혀 씀
const GLOW_DONE: [[f32; 3]; 9] = [
    [19.0, 160.0, 101.0],
    [40.0, 185.0, 125.0],
    [30.0, 150.0, 95.0],
    [15.0, 140.0, 110.0],
    [60.0, 195.0, 130.0],
    [25.0, 170.0, 115.0],
    [80.0, 205.0, 145.0],
    [20.0, 130.0, 95.0],
    [45.0, 180.0, 140.0],
];
const GLOW_FAILED: [[f32; 3]; 9] = [
    [240.0, 68.0, 82.0],
    [255.0, 95.0, 95.0],
    [225.0, 60.0, 75.0],
    [250.0, 120.0, 110.0],
    [255.0, 80.0, 90.0],
    [235.0, 75.0, 95.0],
    [255.0, 110.0, 100.0],
    [210.0, 55.0, 70.0],
    [245.0, 90.0, 105.0],
];
/// 테두리 둘레 조각 (색 번호, 오가는 값 묶음, 밝기를 따를 모서리, 반지름 가로·세로 DIP, 자리 가로·세로 비율)
const GLOW_EDGE: [(usize, u8, &str, f32, f32, f32, f32); 8] = [
    (0, 1, "tl", 80.0, 19.0, 0.27, 0.0),
    (6, 2, "tr", 74.0, 11.0, 0.73, -0.01),
    (7, 3, "tr", 15.0, 44.0, 1.0, 0.33),
    (8, 1, "br", 19.0, 38.0, 1.01, 0.72),
    (4, 2, "br", 84.0, 13.0, 0.67, 1.0),
    (1, 3, "bl", 60.0, 21.0, 0.24, 1.01),
    (2, 1, "bl", 17.0, 40.0, 0.0, 0.60),
    (3, 2, "tl", 13.0, 32.0, -0.01, 0.28),
];
/// 넓게 번지는 조각 (색 번호, 반지름 가로·세로 DIP, 자리 가로·세로 비율)
const GLOW_BLOOM: [(usize, f32, f32, f32, f32); 7] = [
    (0, 110.0, 30.0, 0.27, 0.03),
    (6, 100.0, 20.0, 0.73, 0.01),
    (7, 26.0, 62.0, 1.0, 0.33),
    (8, 30.0, 56.0, 1.01, 0.72),
    (4, 120.0, 22.0, 0.67, 0.99),
    (1, 88.0, 32.0, 0.24, 0.99),
    (2, 28.0, 58.0, 0.0, 0.60),
];
