//! 바와 두 버튼을 하나로 녹여 붙인 모양
//! 모양마다 거리장을 두고 다항식 부드러운 최솟값으로 합친 뒤 0 등고선을 점 목록으로 만듦
//! 간격이 `k` 이상이면 원래 모양 그대로, 마주 보는 두 원은 간격 k/2 이하에서 이어짐

/// 모양 하나, 중심은 모두 같은 가로줄
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Shape {
    /// 가로로 긴 둥근 사각형: 가운데 x, 반너비·반높이, 모서리 반지름
    /// 반높이가 반지름과 같으면 양 끝이 반원인 알약, 열 때 튀어 세로가 더 커지면 옆이 곧음
    Bar { cx: f64, hw: f64, hh: f64, r: f64 },
    /// 타원: 가운데 x, 가로·세로 반지름
    Ellipse { cx: f64, rx: f64, ry: f64 },
}

fn len(x: f64, y: f64) -> f64 {
    (x * x + y * y).sqrt()
}

impl Shape {
    /// 가로 범위 (왼쪽 끝, 오른쪽 끝)
    fn span(&self) -> (f64, f64) {
        match *self {
            Shape::Bar { cx, hw, .. } => (cx - hw, cx + hw),
            Shape::Ellipse { cx, rx, .. } => (cx - rx, cx + rx),
        }
    }

    /// 테두리까지 거리, 안은 음수 (`dy`는 가운데 줄에서), 타원은 근사지만 원이면 정확함
    fn dist(&self, x: f64, dy: f64) -> f64 {
        match *self {
            Shape::Bar { cx, hw, hh, r } => {
                let (qx, qy) = ((x - cx).abs() - (hw - r), dy.abs() - (hh - r));
                len(qx.max(0.0), qy.max(0.0)) + qx.max(qy).min(0.0) - r
            }
            Shape::Ellipse { cx, rx, ry } => (len((x - cx) / rx, dy / ry) - 1.0) * rx.min(ry),
        }
    }

    /// 테두리 바깥 방향 (거리장 기울기), 길이 1
    /// 가운데 줄 위라 방향이 없으면 아래
    fn normal(&self, x: f64, dy: f64) -> (f64, f64) {
        let sy = if dy < 0.0 { -1.0 } else { 1.0 };
        let (nx, ny) = match *self {
            Shape::Bar { cx, hw, hh, r } => {
                let sx = if x < cx { -1.0 } else { 1.0 };
                let (qx, qy) = ((x - cx).abs() - (hw - r), dy.abs() - (hh - r));
                if qx > 0.0 || qy > 0.0 {
                    (qx.max(0.0) * sx, qy.max(0.0) * sy)
                } else if qx > qy {
                    (sx, 0.0)
                } else {
                    (0.0, sy)
                }
            }
            Shape::Ellipse { cx, rx, ry } => ((x - cx) / (rx * rx), dy / (ry * ry)),
        };
        let l = len(nx, ny);
        if l > 1e-9 { (nx / l, ny / l) } else { (0.0, sy) }
    }

    /// 세로줄 `x` 어디서나 이보다 가깝진 않은 거리
    fn reach(&self, x: f64) -> f64 {
        match *self {
            Shape::Bar { cx, hw, r, .. } => {
                let qx = (x - cx).abs() - (hw - r);
                if qx > 0.0 { qx - r } else { f64::NEG_INFINITY }
            }
            Shape::Ellipse { cx, rx, ry } => ((x - cx).abs() / rx - 1.0) * rx.min(ry),
        }
    }

    /// 세로줄 `x`의 이 모양 하나만의 반높이, 밖이면 0
    fn half(&self, x: f64) -> f64 {
        match *self {
            Shape::Bar { cx, hw, hh, r } => {
                let qx = (x - cx).abs() - (hw - r);
                if qx <= 0.0 { hh } else if qx < r { hh - r + (r * r - qx * qx).sqrt() } else { 0.0 }
            }
            Shape::Ellipse { cx, rx, ry } => {
                let u = (x - cx) / rx;
                if u.abs() < 1.0 { ry * (1.0 - u * u).sqrt() } else { 0.0 }
            }
        }
    }
}

/// 모양들을 앞에서부터 차례로 부드럽게 합친 거리 (`dy`는 가운데 줄에서)
/// 같은 쪽을 보는 나란한 테두리 (바 윗변과 버튼 윗변)는 `k`를 줄여 안 부풀게, 마주 보는 곳만 온전한 `k`
fn field(shapes: &[Shape], x: f64, dy: f64, k: f64) -> f64 {
    let mut shapes = shapes.iter();
    let Some(first) = shapes.next() else { return f64::INFINITY };
    let (mut d, mut n) = (first.dist(x, dy), first.normal(x, dy));
    for sh in shapes {
        let (b, nb) = (sh.dist(x, dy), sh.normal(x, dy));
        let kk = k * (0.5 * len(n.0 - nb.0, n.1 - nb.1)).min(1.0);
        // 0이면 앞 모양, 1이면 새 모양, 거리 차가 kk 이상이면 0이나 1이라 정확히 최솟값
        let h = if kk > 1e-6 { (0.5 + 0.5 * (d - b) / kk).clamp(0.0, 1.0) } else if b < d { 1.0 } else { 0.0 };
        d += (b - d) * h - kk * h * (1.0 - h);
        let (nx, ny) = (n.0 + (nb.0 - n.0) * h, n.1 + (nb.1 - n.1) * h);
        let l = len(nx, ny);
        n = if l > 1e-9 { (nx / l, ny / l) } else { nb };
    }
    d
}

/// 가운데 줄 위 `x`에서 녹인 모양들과 그대로 합치는 모양 (`over`)을 합친 거리, 안은 음수
pub(crate) fn center(shapes: &[Shape], over: &[Shape], x: f64, k: f64) -> f64 {
    over.iter().map(|sh| sh.dist(x, 0.0)).fold(field(shapes, x, 0.0, k), f64::min)
}

/// `[a, b]`에서 `f`가 가장 큰 x를 황금분할로 40번 좁혀 찾음, 봉우리가 여럿이면 그중 하나
fn peak(a: f64, b: f64, f: impl Fn(f64) -> f64) -> f64 {
    let g = (5f64.sqrt() - 1.0) / 2.0;
    let (mut a, mut b) = (a, b);
    let (mut c, mut d) = (b - g * (b - a), a + g * (b - a));
    let (mut fc, mut fd) = (f(c), f(d));
    for _ in 0..40 {
        if fc > fd {
            (b, d, fd) = (d, c, fc);
            c = b - g * (b - a);
            fc = f(c);
        } else {
            (a, c, fc) = (c, d, fd);
            d = a + g * (b - a);
            fd = f(d);
        }
    }
    (a + b) / 2.0
}

/// 0 등고선을 이어진 덩어리마다 점 목록으로, 점은 (x, 반높이)이고 첫 점과 끝 점은 반높이 0
/// 가로줄 대칭이고 위아래로 갈수록 거리가 커져서 x마다 반높이 하나면 됨
/// `unit`(1 DIP) 간격으로 훑고, 가파른 양 끝은 이웃 점 반높이 차가 0.4 DIP 안이 될 때까지 반씩 더 나눔
/// `over`는 녹이지 않고 그대로 합치는 모양 (누름으로 부푼 버튼), 이웃과 안 이어지고 경로 하나로 같이 그림
pub fn outline(shapes: &[Shape], over: &[Shape], k: f64, unit: f64) -> Vec<Vec<(f64, f64)>> {
    if shapes.is_empty() && over.is_empty() {
        return Vec::new();
    }
    // 그 줄 어디서나 `k` 넘게 떨어진 모양은 합쳐도 경계를 안 바꿈, 가까운 모양이 하나뿐이면 그 모양 그대로
    // 바 가운데처럼 대부분의 줄이라 이분법을 건너뜀 (다 이분법으로 찾으면 한 번에 1 ms 가까이 듦)
    let lone = |x: f64| {
        let mut near = shapes.iter().filter(|sh| sh.reach(x) < k);
        match (near.next(), near.next()) {
            (first, None) => Some(first),
            _ => None,
        }
    };
    let melted = |x: f64| match lone(x) {
        Some(sh) => sh.is_some_and(|sh| sh.dist(x, 0.0) <= 0.0),
        None => field(shapes, x, 0.0, k) <= 0.0,
    };
    let melted_half = |x: f64| {
        if let Some(sh) = lone(x) {
            return sh.map_or(0.0, |sh| sh.half(x));
        }
        if !melted(x) {
            return 0.0;
        }
        let (mut lo, mut hi) = (0.0, 80.0 * unit);
        for _ in 0..22 {
            let m = (lo + hi) / 2.0;
            if field(shapes, x, m, k) <= 0.0 {
                lo = m;
            } else {
                hi = m;
            }
        }
        lo
    };
    // 세로줄마다 가운데 줄 기준으로 대칭인 구간끼리의 합집합이라 반높이는 큰 쪽
    let inside = |x: f64| melted(x) || over.iter().any(|sh| sh.dist(x, 0.0) <= 0.0);
    let half = |x: f64| over.iter().map(|sh| sh.half(x)).fold(melted_half(x), f64::max);
    let (mut xmin, mut xmax) = (f64::INFINITY, f64::NEG_INFINITY);
    for sh in shapes.iter().chain(over) {
        let (a, b) = sh.span();
        xmin = xmin.min(a);
        xmax = xmax.max(b);
    }
    let (xmin, xmax) = (xmin - k, xmax + k);

    // 훑을 x: 1 DIP 간격에 모양마다 양 끝과 버튼 가운데, 가로로 떨어진 두 모양의 마주 보는 끝 사이에서 합친 거리가 가장 큰 곳
    // 1 DIP보다 좁은 틈은 간격대로만 훑으면 놓쳐서 끊긴 목이 이어진 채 그려짐, 틈이 있으면 가장 큰 곳이 밖
    // 가장 큰 곳은 떨어진 모든 쌍에서 찾아 넣음, 늘어남이 서로 다르면 한가운데에서 비켜남
    // 버튼 꼭대기가 샘플 사이에 오면 반높이가 조금 모자람 (1.25배처럼 칸에 안 맞는 배율)
    let steps = ((xmax - xmin) / unit).ceil() as usize + 1;
    let mut samples: Vec<f64> = (1..=steps).map(|i| xmin + unit * i as f64).collect();
    let spans: Vec<(f64, f64)> = shapes.iter().chain(over).map(Shape::span).collect();
    for sh in shapes.iter().chain(over) {
        if let Shape::Ellipse { cx, .. } = *sh {
            samples.push(cx);
        }
    }
    for &(left, right) in &spans {
        samples.extend([left, right]);
        for &(next, _) in spans.iter().filter(|&&(next, _)| next > right) {
            samples.push(peak(right, next, |x| center(shapes, over, x, k)));
        }
    }
    samples.sort_by(f64::total_cmp);
    samples.dedup_by(|a, b| (*a - *b).abs() < 1e-9);

    // 가운데 줄에서 안에 드는 구간: 경계는 이분법, 안쪽 샘플 x는 따로 모음
    let mut runs: Vec<(f64, Vec<f64>, f64)> = Vec::new();
    let mut cur: Option<(f64, Vec<f64>)> = None;
    let (mut last, mut was) = (xmin, inside(xmin));
    for x in samples {
        let now = inside(x);
        if now != was {
            let (mut a, mut b) = (last, x);
            for _ in 0..24 {
                let m = (a + b) / 2.0;
                if inside(m) == was {
                    a = m;
                } else {
                    b = m;
                }
            }
            let edge = (a + b) / 2.0;
            if now {
                cur = Some((edge, Vec::new()));
            } else if let Some((x0, xs)) = cur.take() {
                runs.push((x0, xs, edge));
            }
        }
        if let (true, Some((_, xs))) = (now, &mut cur) {
            xs.push(x);
        }
        (last, was) = (x, now);
    }

    runs.into_iter()
        .map(|(x0, xs, x1)| {
            let mut pts: Vec<(f64, f64)> = std::iter::once((x0, 0.0)).chain(xs.into_iter().map(|x| (x, half(x)))).chain([(x1, 0.0)]).collect();
            for _ in 0..7 {
                let mut out = Vec::with_capacity(pts.len() * 2);
                out.push(pts[0]);
                let mut added = false;
                for w in pts.windows(2) {
                    let ((xa, ta), (xb, tb)) = (w[0], w[1]);
                    if (tb - ta).abs() > 0.4 * unit && xb - xa > 0.004 * unit {
                        let xm = (xa + xb) / 2.0;
                        out.push((xm, half(xm)));
                        added = true;
                    }
                    out.push(w[1]);
                }
                pts = out;
                if !added {
                    break;
                }
            }
            pts
        })
        .collect()
}
