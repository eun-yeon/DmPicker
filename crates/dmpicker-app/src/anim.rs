//! 애니메이션 기본 도구: 스프링 (열기, 버튼, 패널 높이, 선택 강조, 스크롤, 알림)과 cubic-bezier 트윈 (나머지)

use std::time::Instant;

#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub value: f32,
    pub velocity: f32,
    pub target: f32,
    pub stiffness: f32,
    pub damping: f32,
}

impl Spring {
    pub const fn new(value: f32, stiffness: f32, damping: f32) -> Self {
        Self { value, velocity: 0.0, target: value, stiffness, damping }
    }

    /// 움직이는 도중에 느낌 바꾸기 (stiffness, damping)
    pub fn tune(&mut self, (stiffness, damping): (f32, f32)) {
        self.stiffness = stiffness;
        self.damping = damping;
    }

    pub fn snap(&mut self, v: f32) {
        self.value = v;
        self.target = v;
        self.velocity = 0.0;
    }

    /// 작은 고정 간격으로 나눠 적분함, 프레임 간격이 커도 안정적
    pub fn step(&mut self, dt: f32) {
        let before = self.value;
        let mut left = dt.min(0.1);
        while left > 0.0 {
            let h = left.min(1.0 / 240.0);
            let force = self.stiffness * (self.target - self.value) - self.damping * self.velocity;
            self.velocity += force * h;
            self.value += self.velocity * h;
            left -= h;
        }
        // 4096 px 넘는 스크롤에선 남은 걸음이 f32 반올림에 묻혀 값은 멈추고 속도만 남음
        // 그대로 두면 `settled`가 끝내 참이 안 돼서 계속 그림, 멈추는 거리는 좌표 크기에 비례함
        if self.value == before && (self.target - self.value).abs() < 0.01f32.max(self.target.abs() * 1e-5) {
            self.snap(self.target);
        }
    }

    pub fn settled(&self) -> bool {
        (self.target - self.value).abs() < 0.01 && self.velocity.abs() < 0.01
    }
}

/// 검색창·알림 열기: 띄울 때 0에서 1로 가는 스프링 (stiffness, damping), 살짝 튐
pub const OPEN: (f32, f32) = (340.0, 22.0);
/// 열 때 처음 속도, 첫 프레임부터 움직임
pub const OPEN_KICK: f32 = 4.0;
/// 열 때 스프링과 따로 이 시간 동안 처음엔 빠르게 페이드 인
/// 스프링은 느리게 출발해서 그대로 쓰면 첫 프레임들이 거의 안 보임
pub const OPEN_FADE: f32 = 0.10;

/// 열기 스프링 값 `o`일 때 판 배율 (가로, 세로), 가운데 기준 90 % x 70 %에서 원래 크기로 커짐
pub fn open_scale(o: f32) -> (f32, f32) {
    (0.90 + 0.10 * o, 0.70 + 0.30 * o)
}

/// CSS식 cubic-bezier 이징 곡선
#[derive(Clone, Copy, Debug)]
pub struct Ease(pub f32, pub f32, pub f32, pub f32);

impl Ease {
    /// cubic-bezier(0.22, 1, 0.36, 1): 빠르게 출발해 길고 부드럽게 멈춤, 넘침 없음
    pub const OUT: Ease = Ease(0.22, 1.0, 0.36, 1.0);
    /// cubic-bezier(0.4, 0, 0.2, 1): 기본 in-out
    pub const STANDARD: Ease = Ease(0.4, 0.0, 0.2, 1.0);
    /// cubic-bezier(0, 0, 0.2, 1): 첫 프레임부터 움직이고 천천히 멈춤, 키 입력에 반응하는 것에 씀
    /// in-out 곡선은 처음 30-40 ms 동안 거의 안 움직임
    pub const DECEL: Ease = Ease(0.0, 0.0, 0.2, 1.0);

    pub fn apply(self, x: f32) -> f32 {
        let Ease(p1x, p1y, p2x, p2y) = self;
        let bez = |t: f32, a: f32, b: f32| 3.0 * (1.0 - t) * (1.0 - t) * t * a + 3.0 * (1.0 - t) * t * t * b + t * t * t;
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..24 {
            let mid = (lo + hi) / 2.0;
            if bez(mid, p1x, p2x) < x {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        bez((lo + hi) / 2.0, p1y, p2y)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    delay: f32,
    dur: f32,
    ease: Ease,
}

impl Tween {
    pub fn new(v: f32) -> Self {
        Self { from: v, to: v, start: Instant::now(), delay: 0.0, dur: 0.0, ease: Ease::OUT }
    }

    pub fn snap(&mut self, v: f32) {
        *self = Self::new(v);
    }

    /// 지금 값에서 `to`로 움직임 (이미 그쪽으로 가는 중이면 그대로)
    pub fn go(&mut self, to: f32, secs: f32, ease: Ease) {
        self.go_delayed(to, 0.0, secs, ease);
    }

    pub fn go_delayed(&mut self, to: f32, delay: f32, secs: f32, ease: Ease) {
        if (to - self.to).abs() < 1e-4 {
            return;
        }
        self.from = self.value();
        self.to = to;
        self.start = Instant::now();
        self.delay = delay;
        self.dur = secs.max(1e-3);
        self.ease = ease;
    }

    /// 향하는 값
    pub fn target(&self) -> f32 {
        self.to
    }

    pub fn value(&self) -> f32 {
        let t = ((self.start.elapsed().as_secs_f32() - self.delay) / self.dur).clamp(0.0, 1.0);
        self.from + (self.to - self.from) * self.ease.apply(t)
    }

    pub fn done(&self) -> bool {
        self.start.elapsed().as_secs_f32() >= self.delay + self.dur
    }
}
