//! 게임 위에 뜨는 상태 알림 캡슐, 포커스를 안 가져가고 클릭은 통과함
//! 검색창처럼 제자리에서 열고 닫힘, 살짝 작은 크기에서 튀어 오르듯 커짐

use std::time::{Duration, Instant};

use windows::core::{w, Interface, Result};
use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use windows::core::HSTRING;
use windows::Foundation::{IReference, PropertyValue};
use windows::UI::Composition::{CompositionEllipseGeometry, CompositionPropertySet, CompositionStrokeCap, Compositor, ContainerVisual, ShapeVisual};
use windows_numerics::{Vector2, Vector3};

use crate::anim::{open_scale, Ease, Spring, OPEN, OPEN_FADE, OPEN_KICK};
use crate::beam::{self, Beam, Kind};
use crate::gfx::{self, alpha_step, circle, color, rect, rrect, white, with_alpha, Gfx, BEARING, MARGIN, PLATE_ALPHA, SUB, TEXT, TINT};
use crate::scene::Scene;
use crate::sys;

/// 글 앞의 배지: 없음, 진행률 (0..1)만큼 차는 링, 끝난 결과
/// 끝나면 링이 초록 원으로 튀며 체크가 그려지거나 (Done) 빨간 원에 X가 그려짐 (Failed)
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Badge {
    None,
    Progress(f32),
    Done,
    Failed,
}

impl Badge {
    fn is_end(self) -> bool {
        matches!(self, Badge::Done | Badge::Failed)
    }
}

pub struct Toast {
    hwnd: HWND,
    gfx: Option<Gfx>,
    fonts: Option<[IDWriteTextFormat; 2]>, // 윗줄·아랫줄 글자, gfx와 같이 만들고 버림
    shadow: Option<gfx::Shadow>,           // 캡슐 그림자, gfx와 같이 만들고 버림
    /// 합성기에 올렸으면 진행 링의 호와 빛 (gfx와 같이 만들고 버림)
    lights: Option<Lights>,
    /// 결과 빛을 띄운 끝 배지의 `end_at`, 같은 결과에 두 번 안 띄움
    glowed: Option<Instant>,
    scale: f32,
    text: String,
    badge: Badge,
    ring: Spring,             // 진행 링 채움, `Badge::Progress`를 따라감
    end_at: Option<Instant>,  // 끝 배지로 바뀌기 시작한 때
    until: Option<Instant>,
    open: Spring, // 0 숨김, 1 표시 (가는 길에 살짝 넘침)
    opened_at: Instant, // 페이드 인 시작, 검색창처럼 스프링과 따로 감
    fade_from: f32,     // 페이드 인 시작 불투명도 (닫히던 알림을 다시 열 때)
    shown: bool,  // 창이 보이는지, 닫히는 중에도 true
    last: Instant,
}

/// 캡슐 (양 끝이 반원)
const W: f32 = 384.0;
const H: f32 = 64.0;
/// 글과 바를 둥근 끝에서 띄움
const INSET: f32 = 24.0;
/// 닫는 스프링, 열 땐 검색창처럼 살짝 튀고 (`OPEN`) 닫을 땐 안 튐
const CLOSE: (f32, f32) = (520.0, 46.0);
/// 새 메시지가 지금 것을 바꿀 때 스프링에 주는 반동
const BUMP: f32 = -0.8;
/// 배지: 지름 24 DIP 원, 중심이 캡슐 안쪽으로 이만큼, 글은 그 뒤에
const BADGE: f32 = 24.0;
const BADGE_X: f32 = 32.0;
const BADGE_TEXT_X: f32 = 56.0;
/// 진행 링이 체크로 바뀌는 모션의 곡선과 시간
const MORPH_MS: f32 = 900.0;
const EASE: Ease = Ease(0.25, 0.1, 0.25, 1.0);
const SOFT: Ease = Ease(0.34, 1.35, 0.64, 1.0);
const POP: Ease = Ease(0.34, 1.96, 0.94, 1.0);
const LAND: Ease = Ease(0.14, 2.56, 0.94, 1.0);
/// 24 DIP 배지 박스 안의 체크 (획 2, 둥근 끝)
const CHECK: [(f32, f32); 3] = [(8.0, 12.5), (10.8, 15.5), (16.4, 9.5)];
/// 실패 때 같은 박스에 그리는 X, 한 획씩: 오른쪽 위에서 왼쪽 아래, 다음은 왼쪽 위에서 오른쪽 아래
const CROSS: [[(f32, f32); 2]; 2] = [[(15.5, 8.5), (8.5, 15.5)], [(8.5, 8.5), (15.5, 15.5)]];
/// 끝 배지 원: 차가운 초록과 빨강, 둘 다 캡슐과도 위의 흰 표시와도 3:1 이상
const GREEN: D2D1_COLOR_F = D2D1_COLOR_F { r: 0.075, g: 0.627, b: 0.396, a: 1.0 }; // #13A065
const RED: D2D1_COLOR_F = D2D1_COLOR_F { r: 0.941, g: 0.267, b: 0.322, a: 1.0 }; // #F04452
/// 진행 링과 트랙, 글처럼 이 불투명도의 흰색
const RING: f32 = 0.85;
const TRACK: f32 = 0.14;
/// 진행 링 굵기 (DIP), 합성기가 그리는 호와 트랙이 같이 씀
const RING_STROKE: f32 = 2.5;
/// 진행 링이 다 찬 것으로 보는 채움, 완료면 여기서 체크로 바뀜
const FULL: f32 = 0.995;

/// 모션이 `t` ms 지났을 때 (트랙 링, 진행 링, 원) 불투명도, 배지 배율,
/// 뛰어오른 높이 (DIP, 위가 음수), 체크가 그려진 비율
fn morph(t: f32) -> (f32, f32, f32, f32, f32, f32) {
    let at = |ms: f32| (t / ms).clamp(0.0, 1.0);
    let hop = if t < 250.0 {
        -3.0 * SOFT.apply(at(250.0))
    } else {
        -3.0 * (1.0 - LAND.apply(((t - 250.0) / 300.0).clamp(0.0, 1.0)))
    };
    // 시작 전엔 안 그림, 길이 0인 획도 둥근 끝이 점으로 찍힘
    let check = if t <= 230.0 { 0.0 } else { Ease::OUT.apply(((t - 230.0) / 600.0).min(1.0)) };
    let disc = SOFT.apply(at(350.0)).clamp(0.0, 1.0);
    (1.0 - EASE.apply(at(245.0)), 1.0 - EASE.apply(at(175.0)), disc, 1.0 + 0.09 * POP.apply(at(350.0)), hop, check)
}

/// 진행 링 스프링 (300, 34)과 같은 합성기 스프링: 감쇠비 34 / (2√300), 감쇠 없을 때 주기 2π / √300
const ARC_DAMPING: f32 = 0.9815;
const ARC_PERIOD: f32 = 0.3628;
/// 결과 빛을 보여 주는 초, 그 뒤 0.5초 동안 사라짐
const GLOW_SECS: f32 = 2.3;

/// 합성기에 올린 알림의 빛: 차오르는 진행 링의 호, 인식 중 테두리를 도는 빛, 결과 빛 (완료·실패)
/// 호는 진행률이 바뀔 때마다 링 스프링과 같은 스프링으로 합성기가 그림, 그동안 앱은 안 그림
struct Lights {
    compositor: Compositor,
    over: ContainerVisual,
    arc: ShapeVisual,
    geo: CompositionEllipseGeometry,
    props: CompositionPropertySet,
    size: f32,
    spin: Beam,
    done: Beam,
    failed: Beam,
}

impl Drop for Lights {
    fn drop(&mut self) {
        let _ = self.over.Children().and_then(|c| c.Remove(&self.arc));
    }
}

impl Lights {
    fn new(scene: &Scene, s: f32) -> Result<Self> {
        let c = &scene.compositor;
        let size = (BADGE + 4.0) * s;
        let geo = c.CreateEllipseGeometry()?;
        let r = (BADGE - RING_STROKE) / 2.0 * s;
        geo.SetCenter(Vector2 { X: size / 2.0, Y: size / 2.0 })?;
        geo.SetRadius(Vector2 { X: r, Y: r })?;
        let shape = c.CreateSpriteShapeWithGeometry(&geo)?;
        shape.SetStrokeBrush(&c.CreateColorBrushWithColor(beam::rgb([255.0; 3], RING))?)?;
        shape.SetStrokeThickness(RING_STROKE * s)?;
        shape.SetStrokeStartCap(CompositionStrokeCap::Round)?;
        shape.SetStrokeEndCap(CompositionStrokeCap::Round)?;
        let arc = c.CreateShapeVisual()?;
        arc.SetSize(Vector2 { X: size, Y: size })?;
        arc.Shapes()?.Append(&shape)?;
        // 타원 둘레가 12시에서 시계 방향으로 시작해서 안 돌림, 가운데는 배율 기준
        arc.SetCenterPoint(Vector3 { X: size / 2.0, Y: size / 2.0, Z: 0.0 })?;
        let props = c.CreatePropertySet()?;
        props.InsertScalar(&HSTRING::from("alpha"), 1.0)?;
        // 다 비었으면 둥근 끝이 점으로 찍히지 않게 숨김
        let e = c.CreateExpressionAnimationWithExpression(&HSTRING::from("g.TrimEnd > 0.002 ? p.alpha : 0"))?;
        e.SetReferenceParameter(&HSTRING::from("g"), &geo)?;
        e.SetReferenceParameter(&HSTRING::from("p"), &props)?;
        arc.StartAnimation(&HSTRING::from("Opacity"), &e)?;
        arc.SetIsVisible(false)?;
        scene.over.Children()?.InsertAtTop(&arc)?;
        let (w, h) = (W * s, H * s);
        Ok(Self {
            compositor: c.clone(),
            over: scene.over.clone(),
            arc,
            geo,
            props,
            size,
            spin: Beam::new(scene, Kind::Spin, w, h, h / 2.0, s)?,
            done: Beam::new(scene, Kind::Glow { failed: false }, w, h, h / 2.0, s)?,
            failed: Beam::new(scene, Kind::Glow { failed: true }, w, h, h / 2.0, s)?,
        })
    }

    /// 호를 `value` 길이 (0..1)로 바로 맞춤
    fn snap(&self, value: f32) -> Result<()> {
        self.geo.StopAnimation(&HSTRING::from("TrimEnd"))?;
        self.geo.SetTrimEnd(value.clamp(0.0, 1.0))
    }

    /// 링 스프링의 지금 값·속도에서 목표까지를 합성기 스프링으로 이어 감
    fn aim(&self, ring: &Spring) -> Result<()> {
        let a = self.compositor.CreateSpringScalarAnimation()?;
        a.SetDampingRatio(ARC_DAMPING)?;
        a.SetPeriod(beam::ts(ARC_PERIOD))?;
        a.SetInitialValue(&PropertyValue::CreateSingle(ring.value)?.cast::<IReference<f32>>()?)?;
        a.SetInitialVelocity(ring.velocity)?;
        a.SetFinalValue(&PropertyValue::CreateSingle(ring.target)?.cast::<IReference<f32>>()?)?;
        self.geo.StartAnimation(&HSTRING::from("TrimEnd"), &a)
    }

    /// 새 메시지의 배지를 따라감: 진행 링, 인식 중 도는 빛, 끝 배지가 아니면 지난 결과 빛은 바로 끔
    fn follow(&self, badge: Badge, snap: bool, ring: &Spring) -> Result<()> {
        match badge {
            Badge::Progress(_) if snap => self.snap(ring.value)?,
            Badge::Progress(_) | Badge::Done => self.aim(ring)?,
            _ => {}
        }
        if !badge.is_end() {
            self.done.stop();
            self.failed.stop();
        }
        self.spin.set(matches!(badge, Badge::Progress(_)))
    }

    /// 결과 배지 모션이 시작될 때, 도는 빛과 반대 결과 빛을 끄고 결과 빛을 한 번 보여 줌
    fn ended(&self, failed: bool) -> Result<()> {
        self.spin.set(false)?;
        let (show, other) = if failed { (&self.failed, &self.done) } else { (&self.done, &self.failed) };
        other.stop();
        show.once(GLOW_SECS)
    }

    /// 캡슐 (`at`, 창 px)과 불투명도, 배지 가운데와 배율에 맞춰 놓음
    fn place(&self, at: D2D_RECT_F, alpha: f32, badge: (f32, f32), scale: f32, arc_live: bool) -> Result<()> {
        for b in [&self.spin, &self.done, &self.failed] {
            b.place(at, alpha)?;
        }
        self.arc.SetIsVisible(arc_live)?;
        self.arc.SetOffset(Vector3 { X: badge.0 - self.size / 2.0, Y: badge.1 - self.size / 2.0, Z: 0.0 })?;
        self.arc.SetScale(Vector3 { X: scale, Y: scale, Z: 1.0 })?;
        self.props.InsertScalar(&HSTRING::from("alpha"), alpha)
    }

    fn settle(&self) {
        for b in [&self.spin, &self.done, &self.failed] {
            b.settle();
        }
    }

    /// 알림을 숨길 때 바로 다 숨김
    fn stop(&self) {
        for b in [&self.spin, &self.done, &self.failed] {
            b.stop();
        }
        let _ = self.arc.SetIsVisible(false);
    }
}

impl Toast {
    pub fn new(wndproc: WNDPROC) -> Result<Self> {
        let ex = WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED | WS_EX_NOREDIRECTIONBITMAP;
        let hwnd = sys::create_popup(w!("DmPickerToast"), &format!("{} status", crate::APP_NAME), ex, wndproc)?;
        unsafe {
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
        }
        Ok(Self {
            hwnd,
            gfx: None,
            fonts: None,
            shadow: None,
            lights: None,
            glowed: None,
            scale: 1.0,
            text: String::new(),
            badge: Badge::None,
            ring: Spring::new(0.0, 300.0, 34.0),
            end_at: None,
            until: None,
            open: Spring::new(0.0, OPEN.0, OPEN.1),
            opened_at: Instant::now(),
            fade_from: 0.0,
            shown: false,
            last: Instant::now(),
        })
    }

    /// `badge` 뒤에 `text` 표시, `secs` 뒤 자동으로 숨김 (None이면 계속 표시)
    pub fn show(&mut self, text: &str, badge: Badge, secs: Option<f32>) {
        let opening = !self.shown || self.open.target < 1.0;
        let bump = !opening && badge == Badge::None && text != self.text;
        let snap = opening || !matches!(self.badge, Badge::Progress(_));
        match badge {
            Badge::Progress(p) if snap => self.ring.snap(p),
            Badge::Progress(p) => self.ring.target = p,
            // 진행 중에 끝나면 링이 다 찬 걸 보여 준 뒤 체크로 (`frame`)
            Badge::Done if self.badge != badge => {
                let filling = matches!(self.badge, Badge::Progress(_)) && !opening;
                self.ring.target = 1.0;
                self.end_at = if filling && self.ring.value < FULL { None } else { Some(Instant::now()) };
            }
            Badge::Failed if self.badge != badge => self.end_at = Some(Instant::now()),
            _ => {}
        }
        if !badge.is_end() {
            self.end_at = None;
            self.glowed = None;
        }
        self.text = text.to_string();
        self.badge = badge;
        self.until = secs.map(|s| Instant::now() + Duration::from_secs_f32(s));
        let game = sys::game_rect();
        let anchor = game.unwrap_or_else(sys::cursor_work_area);
        let scale = sys::ui_scale(&anchor, game.is_some());
        // 조금 다르면 그대로 둠, 창과 swapchain이 늘 같은 배율로 만들어짐
        if (scale - self.scale).abs() > 0.01 {
            self.scale = scale;
            self.drop_gfx();
        }
        let scale = self.scale;
        let (w, h) = (((W + 2.0 * MARGIN) * scale) as i32, ((H + 2.0 * MARGIN) * scale) as i32);
        let x = (anchor.left + anchor.right) / 2 - w / 2;
        // 캡슐 위 끝이 게임 위에서 24, 창은 여백만큼 게임 위로 나감 (클릭이 통과해서 괜찮음)
        let y = anchor.top + ((24.0 - MARGIN) * scale).floor() as i32;
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
        match &self.gfx {
            None => {
                self.gfx = Gfx::new(self.hwnd, w as u32, h as u32, gfx::TOAST_MAX_FPS).ok();
                self.fonts = self.gfx.as_ref().and_then(|g| {
                    let f = |size: f32| g.ui_format(size * scale, DWRITE_FONT_WEIGHT_MEDIUM).ok();
                    Some([f(14.0)?, f(11.0)?])
                });
                self.shadow = self.gfx.as_ref().and_then(|g| gfx::Shadow::new(g, scale).ok());
                if self.fonts.is_none() || self.shadow.is_none() {
                    self.drop_gfx(); // 글꼴·그림자를 못 만들면 다음 알림에서 처음부터 다시
                }
                // 빛을 못 만들면 링은 앱이 그리고 빛은 없음
                self.lights = self.gfx.as_ref().and_then(|g| g.scene()).and_then(|sc| Lights::new(sc, scale).inspect_err(|e| eprintln!("toast lights: {e:?}")).ok());
            }
            Some(g) if opening => g.retune(self.hwnd),
            Some(_) => {}
        }
        if opening {
            // 빈 상태나 닫히던 알림이 아직 보이던 상태에서 시작
            self.fade_from = if self.shown { self.open.value.clamp(0.0, 1.0) } else { 0.0 };
            self.opened_at = Instant::now();
            if !self.shown {
                self.open.snap(0.0);
                self.open.velocity = OPEN_KICK;
            }
            self.open.tune(OPEN);
            self.open.target = 1.0;
        } else if bump {
            self.open.velocity = BUMP;
        }
        if let Some(Err(e)) = self.lights.as_ref().map(|l| l.follow(badge, snap, &self.ring)) {
            eprintln!("toast lights: {e:?}");
        }
        self.shown = true;
        self.last = Instant::now();
        self.render();
    }

    fn drop_gfx(&mut self) {
        self.lights = None;
        self.gfx = None;
        self.fonts = None;
        self.shadow = None;
    }

    /// 다 사라진 빛의 움직임을 멈춤 (주기 타이머에서 부름)
    pub fn settle(&self) {
        if let Some(l) = &self.lights {
            l.settle();
        }
    }

    /// 링이 차오르는 걸 합성기가 그리는 중 (끝 배지 모션 전까지)
    fn arc_live(&self) -> bool {
        self.lights.is_some() && self.end_at.is_none() && matches!(self.badge, Badge::Progress(_) | Badge::Done)
    }

    /// 앱이 다시 그려야 하는 움직임: 열고 닫기, 페이드 인, 끝 배지 모션, 합성기 없이 차오르는 링
    fn needs_draw(&self) -> bool {
        let morphing = self.end_at.is_some_and(|t| t.elapsed().as_secs_f32() * 1000.0 < MORPH_MS);
        let filling = self.badge == Badge::Done && self.end_at.is_none();
        let fading = self.opened_at.elapsed().as_secs_f32() < OPEN_FADE;
        !self.open.settled() || fading || morphing || (!self.arc_live() && (!self.ring.settled() || filling))
    }

    /// 다시 줄어들며 닫히기 시작
    pub fn hide(&mut self) {
        self.until = None;
        if self.shown {
            self.open.tune(CLOSE);
            self.open.target = 0.0;
            self.last = Instant::now(); // 그동안 스프링이 쉬었으니 지금부터 시간을 잼
        }
    }

    /// 프레임마다 볼 게 있음, 링 스프링은 합성기가 그려도 끝 배지로 넘어갈 때를 알려고 따라감
    pub fn animating(&self) -> bool {
        let filling = self.badge == Badge::Done && self.end_at.is_none(); // 0.99에서 멈춘 링도 끝까지
        self.shown && (self.needs_draw() || !self.ring.settled() || filling)
    }

    /// 애니메이션 한 프레임 ([`Toast::animating`] 동안 UI 루프에서 부름)
    pub fn frame(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32();
        self.open.step(dt);
        self.ring.step(dt);
        if self.badge == Badge::Done && self.end_at.is_none() && self.ring.value >= FULL {
            self.end_at = Some(now);
        }
        self.last = now;
        if self.open.target == 0.0 && self.open.value < 0.02 {
            self.shown = false;
            if let Some(l) = &self.lights {
                l.stop();
            }
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            return;
        }
        // 링만 차오르는 동안엔 합성기가 그려서 앱은 안 그림
        if self.needs_draw() {
            self.render();
        }
    }

    /// 주기 타이머에서 부름
    pub fn tick(&mut self) {
        if let Some(t) = self.until {
            if Instant::now() >= t {
                self.hide();
            }
        }
    }

    /// 한 프레임 그림, 그래픽 장치가 사라졌으면 (`gfx::device_lost`) 버림, 다음 `show`가 새로 만듦
    fn render(&mut self) {
        let r = self.draw_frame();
        if let Err(e) = r {
            eprintln!("toast render: {e:?}");
            if gfx::device_lost(&e) {
                self.drop_gfx();
            }
        }
    }

    fn draw_frame(&mut self) -> Result<()> {
        let arc_live = self.arc_live();
        let (Some(g), Some([f1, f2]), Some(shadow)) = (&self.gfx, &self.fonts, &self.shadow) else { return Ok(()) };
        let s = self.scale;
        let o = self.open.value;
        // 검색창처럼 가운데 기준으로 커지며 (`open_scale`) 빠르게 페이드 인
        let (sx, sy) = open_scale(o);
        let (cx, cy) = ((W / 2.0 + MARGIN) * s, (H / 2.0 + MARGIN) * s);
        let (w, h) = (W * s * sx, H * s * sy);
        let shape = rrect(cx - w / 2.0 + 0.5, cy - h / 2.0 + 0.5, cx + w / 2.0 - 0.5, cy + h / 2.0 - 0.5, (h - 1.0) / 2.0);
        let alpha = if self.open.target > 0.0 {
            let t = Ease::OUT.apply((self.opened_at.elapsed().as_secs_f32() / OPEN_FADE).min(1.0));
            self.fade_from + (1.0 - self.fade_from) * t
        } else {
            o.clamp(0.0, 1.0)
        };
        // 내용은 같은 중심으로 캡슐 너비만큼 커짐 (늘 캡슐 안)
        let ride = windows_numerics::Matrix3x2 { M11: sx, M12: 0.0, M21: 0.0, M22: sx, M31: cx * (1.0 - sx), M32: cy * (1.0 - sx) };
        let (left, top, cw, ch) = (MARGIN * s, MARGIN * s, W * s, H * s);
        let (line1, line2) = match self.text.split_once('\n') {
            Some((a, b)) => (a, Some(b)),
            None => (self.text.as_str(), None),
        };
        unsafe {
            let dc = &g.dc;
            // 그림자 입력: 캡슐 모양을 흰색으로 채움, 크기가 바뀌어서 프레임마다 새로 기록
            let target = dc.GetTarget()?;
            let _home = gfx::Retarget(dc, target.clone());
            let mask = dc.CreateCommandList()?;
            dc.SetTarget(&mask);
            let d = gfx::Drawing::begin(dc, &windows_numerics::Matrix3x2::identity());
            dc.FillRoundedRectangle(&shape, &g.brush(white(1.0))?);
            d.end()?;
            mask.Close()?;
            shadow.set_input_image(&mask.cast()?);

            dc.SetTarget(&target);
            let d = gfx::Drawing::begin(dc, &windows_numerics::Matrix3x2::identity());
            dc.Clear(Some(&color(0.0, 0.0, 0.0, 0.0)));
            // 그림자도 판과 같은 레이어 안이라 같이 페이드
            dc.PushLayer(&gfx::layer(alpha, None), None);
            dc.DrawImage(&shadow.output()?, None, None, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_COMPOSITE_MODE_SOURCE_OVER);
            dc.FillRoundedRectangle(&shape, &g.brush(with_alpha(TINT, PLATE_ALPHA))?);
            // 테두리 선, 위쪽이 더 밝음
            dc.DrawRoundedRectangle(&shape, &g.rim_gradient(shape.rect.top, shape.rect.bottom, 1.0)?, 1.0, None);

            {
                let clip: ID2D1Geometry = g.factory.CreateRoundedRectangleGeometry(&shape)?.cast()?;
                dc.PushLayer(&gfx::layer(1.0, Some(&clip)), None);
                dc.SetTransform(&ride);
                let lead = if self.badge == Badge::None { INSET } else { BADGE_TEXT_X };
                let (x0, tw) = (left + lead * s - BEARING, cw - (lead + INSET) * s);
                let t1 = g.brush(TEXT)?;
                let t2 = g.brush(SUB)?;
                match line2 {
                    Some(l2) => {
                        let (y1, y2) = (15.5, 36.0);
                        let a = g.layout(line1, f1, tw, 20.0 * s)?;
                        dc.DrawTextLayout(gfx::pt(x0, top + (y1 * s).round()), &a, &t1, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                        let b = g.layout(l2, f2, tw, 16.0 * s)?;
                        dc.DrawTextLayout(gfx::pt(x0, top + (y2 * s).round()), &b, &t2, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                    }
                    None => {
                        let a = g.layout(line1, f1, tw, ch)?;
                        a.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                        dc.DrawTextLayout(gfx::pt(x0, top), &a, &t1, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
                    }
                }
                if self.badge != Badge::None {
                    self.draw_badge(g, (left + BADGE_X * s, top + ch / 2.0), &ride, arc_live)?;
                }
                dc.SetTransform(&windows_numerics::Matrix3x2::identity());
                dc.PopLayer();
            }
            dc.PopLayer();
            d.end()?;
        }
        // 빛이 안 돼도 알림은 띄움
        if let Some(l) = &self.lights {
            // 결과 배지 모션이 시작될 때 결과 빛을 한 번
            if self.end_at.is_some() && self.glowed != self.end_at {
                self.glowed = self.end_at;
                if let Err(e) = l.ended(self.badge == Badge::Failed) {
                    eprintln!("toast lights: {e:?}");
                }
            }
            let at = rect(shape.rect.left - 0.5, shape.rect.top - 0.5, shape.rect.right + 0.5, shape.rect.bottom + 0.5);
            // 배지 가운데를 캡슐과 같이 키움 (`ride`)
            let (bx, by) = (left + BADGE_X * s, top + ch / 2.0);
            if let Err(e) = l.place(at, alpha, (cx + (bx - cx) * sx, cy + (by - cy) * sx), sx, arc_live) {
                eprintln!("toast lights: {e:?}");
            }
        }
        self.gfx.as_ref().unwrap().present()
    }

    /// `c`를 중심으로 배지를 그림 (카드 좌표, `ride`가 창 좌표로 바꿈)
    /// `arc_live`면 차오르는 호는 합성기가 그려서 트랙만 그림
    unsafe fn draw_badge(&self, g: &Gfx, c: (f32, f32), ride: &windows_numerics::Matrix3x2, arc_live: bool) -> Result<()> {
        let s = self.scale;
        let dc = &g.dc;
        let t = self.end_at.map_or(0.0, |t| t.elapsed().as_secs_f32() * 1000.0);
        let (track_a, ring_a, disc_a, scale, hop, check) =
            if self.end_at.is_some() { morph(t) } else { (1.0, 1.0, 0.0, 1.0, 0.0, 0.0) };
        // 배지 중심 기준으로 커지고 뛰어오른 뒤 캡슐을 따라감
        let local = windows_numerics::Matrix3x2 {
            M11: scale,
            M12: 0.0,
            M21: 0.0,
            M22: scale,
            M31: c.0 * (1.0 - scale),
            M32: c.1 * (1.0 - scale) + hop * s,
        };
        dc.SetTransform(&(local * ride));
        let (stroke, r) = (RING_STROKE * s, (BADGE - RING_STROKE) / 2.0 * s);
        if track_a > 0.0 {
            dc.DrawEllipse(&circle(c.0, c.1, r), &g.brush(white(alpha_step(TRACK * track_a)))?, stroke, None);
        }
        let round = g.factory.CreateStrokeStyle(
            &D2D1_STROKE_STYLE_PROPERTIES1 {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                lineJoin: D2D1_LINE_JOIN_ROUND,
                ..Default::default()
            },
            None,
        )?;
        let fill = self.ring.value.clamp(0.0, 1.0);
        if ring_a > 0.0 && fill > 0.002 && !arc_live {
            let ring_b = g.brush(white(alpha_step(RING * ring_a)))?;
            if fill > 0.998 {
                dc.DrawEllipse(&circle(c.0, c.1, r), &ring_b, stroke, None);
            } else {
                // 12시 방향부터 시계 방향
                let a = fill * std::f32::consts::TAU;
                let path = g.factory.CreatePathGeometry()?;
                let sink = path.Open()?;
                sink.BeginFigure(gfx::pt(c.0, c.1 - r), D2D1_FIGURE_BEGIN_HOLLOW);
                sink.AddArc(&D2D1_ARC_SEGMENT {
                    point: gfx::pt(c.0 + r * a.sin(), c.1 - r * a.cos()),
                    size: D2D_SIZE_F { width: r, height: r },
                    rotationAngle: 0.0,
                    sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                    arcSize: if fill > 0.5 { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                });
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
                sink.Close()?;
                dc.DrawGeometry(&path, &ring_b, stroke, &round);
            }
        }
        if disc_a > 0.0 {
            let disc = if self.badge == Badge::Failed { RED } else { GREEN };
            dc.FillEllipse(&circle(c.0, c.1, (BADGE / 2.0 + 1.0) * s), &g.brush(with_alpha(disc, alpha_step(disc_a)))?);
        }
        if check > 0.0 {
            // 글리프 길이의 앞 `check`만큼, 획 순서대로
            let at = |(x, y): (f32, f32)| gfx::pt(c.0 + (x - BADGE / 2.0) * s, c.1 + (y - BADGE / 2.0) * s);
            let strokes: Vec<Vec<windows_numerics::Vector2>> = if self.badge == Badge::Failed {
                CROSS.iter().map(|st| st.iter().map(|&p| at(p)).collect()).collect()
            } else {
                vec![CHECK.iter().map(|&p| at(p)).collect()]
            };
            let seg = |a: windows_numerics::Vector2, b: windows_numerics::Vector2| ((b.X - a.X).powi(2) + (b.Y - a.Y).powi(2)).sqrt();
            let total: f32 = strokes.iter().flat_map(|st| st.windows(2).map(|w| seg(w[0], w[1]))).sum();
            let mut left = check * total;
            let path = g.factory.CreatePathGeometry()?;
            let sink = path.Open()?;
            for st in &strokes {
                if left <= 0.0 {
                    break;
                }
                sink.BeginFigure(st[0], D2D1_FIGURE_BEGIN_HOLLOW);
                for w in st.windows(2) {
                    let l = seg(w[0], w[1]);
                    let k = (left / l).min(1.0);
                    sink.AddLine(gfx::pt(w[0].X + (w[1].X - w[0].X) * k, w[0].Y + (w[1].Y - w[0].Y) * k));
                    left -= l;
                    if left <= 0.0 {
                        break;
                    }
                }
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
            }
            sink.Close()?;
            dc.DrawGeometry(&path, &g.brush(white(1.0))?, 2.0 * s, &round);
        }
        dc.SetTransform(ride);
        Ok(())
    }
}
