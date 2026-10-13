//! 아이콘: 256 단위 정사각형에 그린 채움 경로, 그릴 자리의 상자 크기로 줄여 채움 (라이선스는 licenses 폴더)

use windows::core::{Error, Result};
use windows::Win32::Foundation::E_INVALIDARG;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;

/// 경로 좌표의 한 변, 가운데가 (128, 128)
pub const VIEW: f32 = 256.0;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Icon {
    /// 히스토리 버튼
    History,
    /// 설정 버튼
    Settings,
    /// 무작위로 고르기 줄
    Random,
    /// 히스토리의 플레이리스트 줄
    Playlist,
    /// 입력 칸 앞, 검색
    Search,
    /// 입력 칸 앞, 키워드 입력
    Keyword,
    /// 즐겨찾기 꺼짐 (테두리)
    StarOff,
    /// 즐겨찾기 켜짐 (채움)
    StarOn,
    /// 키워드 칩 지우기
    Remove,
}

impl Icon {
    /// SVG 경로 데이터
    fn path(self) -> &'static str {
        match self {
            Icon::History => "M208,88H48a16,16,0,0,0-16,16v96a16,16,0,0,0,16,16H208a16,16,0,0,0,16-16V104A16,16,0,0,0,208,88Zm0,112H48V104H208v96ZM48,64a8,8,0,0,1,8-8H200a8,8,0,0,1,0,16H56A8,8,0,0,1,48,64ZM64,32a8,8,0,0,1,8-8H184a8,8,0,0,1,0,16H72A8,8,0,0,1,64,32Z",
            Icon::Settings => "M128,80a48,48,0,1,0,48,48A48.05,48.05,0,0,0,128,80Zm0,80a32,32,0,1,1,32-32A32,32,0,0,1,128,160Zm88-29.84q.06-2.16,0-4.32l14.92-18.64a8,8,0,0,0,1.48-7.06,107.21,107.21,0,0,0-10.88-26.25,8,8,0,0,0-6-3.93l-23.72-2.64q-1.48-1.56-3-3L186,40.54a8,8,0,0,0-3.94-6,107.71,107.71,0,0,0-26.25-10.87,8,8,0,0,0-7.06,1.49L130.16,40Q128,40,125.84,40L107.2,25.11a8,8,0,0,0-7.06-1.48A107.6,107.6,0,0,0,73.89,34.51a8,8,0,0,0-3.93,6L67.32,64.27q-1.56,1.49-3,3L40.54,70a8,8,0,0,0-6,3.94,107.71,107.71,0,0,0-10.87,26.25,8,8,0,0,0,1.49,7.06L40,125.84Q40,128,40,130.16L25.11,148.8a8,8,0,0,0-1.48,7.06,107.21,107.21,0,0,0,10.88,26.25,8,8,0,0,0,6,3.93l23.72,2.64q1.49,1.56,3,3L70,215.46a8,8,0,0,0,3.94,6,107.71,107.71,0,0,0,26.25,10.87,8,8,0,0,0,7.06-1.49L125.84,216q2.16.06,4.32,0l18.64,14.92a8,8,0,0,0,7.06,1.48,107.21,107.21,0,0,0,26.25-10.88,8,8,0,0,0,3.93-6l2.64-23.72q1.56-1.48,3-3L215.46,186a8,8,0,0,0,6-3.94,107.71,107.71,0,0,0,10.87-26.25,8,8,0,0,0-1.49-7.06Zm-16.1-6.5a73.93,73.93,0,0,1,0,8.68,8,8,0,0,0,1.74,5.48l14.19,17.73a91.57,91.57,0,0,1-6.23,15L187,173.11a8,8,0,0,0-5.1,2.64,74.11,74.11,0,0,1-6.14,6.14,8,8,0,0,0-2.64,5.1l-2.51,22.58a91.32,91.32,0,0,1-15,6.23l-17.74-14.19a8,8,0,0,0-5-1.75h-.48a73.93,73.93,0,0,1-8.68,0,8,8,0,0,0-5.48,1.74L100.45,215.8a91.57,91.57,0,0,1-15-6.23L82.89,187a8,8,0,0,0-2.64-5.1,74.11,74.11,0,0,1-6.14-6.14,8,8,0,0,0-5.1-2.64L46.43,170.6a91.32,91.32,0,0,1-6.23-15l14.19-17.74a8,8,0,0,0,1.74-5.48,73.93,73.93,0,0,1,0-8.68,8,8,0,0,0-1.74-5.48L40.2,100.45a91.57,91.57,0,0,1,6.23-15L69,82.89a8,8,0,0,0,5.1-2.64,74.11,74.11,0,0,1,6.14-6.14A8,8,0,0,0,82.89,69L85.4,46.43a91.32,91.32,0,0,1,15-6.23l17.74,14.19a8,8,0,0,0,5.48,1.74,73.93,73.93,0,0,1,8.68,0,8,8,0,0,0,5.48-1.74L155.55,40.2a91.57,91.57,0,0,1,15,6.23L173.11,69a8,8,0,0,0,2.64,5.1,74.11,74.11,0,0,1,6.14,6.14,8,8,0,0,0,5.1,2.64l22.58,2.51a91.32,91.32,0,0,1,6.23,15l-14.19,17.74A8,8,0,0,0,199.87,123.66Z",
            Icon::Random => "M237.66,178.34a8,8,0,0,1,0,11.32l-24,24a8,8,0,0,1-11.32-11.32L212.69,192H200.94a72.12,72.12,0,0,1-58.59-30.15l-41.72-58.4A56.1,56.1,0,0,0,55.06,80H32a8,8,0,0,1,0-16H55.06a72.12,72.12,0,0,1,58.59,30.15l41.72,58.4A56.1,56.1,0,0,0,200.94,176h11.75l-10.35-10.34a8,8,0,0,1,11.32-11.32ZM143,107a8,8,0,0,0,11.16-1.86l1.2-1.67A56.1,56.1,0,0,1,200.94,80h11.75L202.34,90.34a8,8,0,0,0,11.32,11.32l24-24a8,8,0,0,0,0-11.32l-24-24a8,8,0,0,0-11.32,11.32L212.69,64H200.94a72.12,72.12,0,0,0-58.59,30.15l-1.2,1.67A8,8,0,0,0,143,107Zm-30,42a8,8,0,0,0-11.16,1.86l-1.2,1.67A56.1,56.1,0,0,1,55.06,176H32a8,8,0,0,0,0,16H55.06a72.12,72.12,0,0,0,58.59-30.15l1.2-1.67A8,8,0,0,0,113,149Z",
            Icon::Playlist => "M32,64a8,8,0,0,1,8-8H216a8,8,0,0,1,0,16H40A8,8,0,0,1,32,64Zm8,72H160a8,8,0,0,0,0-16H40a8,8,0,0,0,0,16Zm72,48H40a8,8,0,0,0,0,16h72a8,8,0,0,0,0-16Zm135.66-57.7a8,8,0,0,1-10,5.36L208,122.75V192a32.05,32.05,0,1,1-16-27.69V112a8,8,0,0,1,10.3-7.66l40,12A8,8,0,0,1,247.66,126.3ZM192,192a16,16,0,1,0-16,16A16,16,0,0,0,192,192Z",
            Icon::Search => "M229.66,218.34l-50.07-50.06a88.11,88.11,0,1,0-11.31,11.31l50.06,50.07a8,8,0,0,0,11.32-11.32ZM40,112a72,72,0,1,1,72,72A72.08,72.08,0,0,1,40,112Z",
            Icon::Keyword => "M224,88H175.4l8.47-46.57a8,8,0,0,0-15.74-2.86l-9,49.43H111.4l8.47-46.57a8,8,0,0,0-15.74-2.86L95.14,88H48a8,8,0,0,0,0,16H92.23L83.5,152H32a8,8,0,0,0,0,16H80.6l-8.47,46.57a8,8,0,0,0,6.44,9.3A7.79,7.79,0,0,0,80,224a8,8,0,0,0,7.86-6.57l9-49.43H144.6l-8.47,46.57a8,8,0,0,0,6.44,9.3A7.79,7.79,0,0,0,144,224a8,8,0,0,0,7.86-6.57l9-49.43H208a8,8,0,0,0,0-16H163.77l8.73-48H224a8,8,0,0,0,0-16Zm-76.5,64H99.77l8.73-48h47.73Z",
            Icon::StarOff => "M239.18,97.26A16.38,16.38,0,0,0,224.92,86l-59-4.76L143.14,26.15a16.36,16.36,0,0,0-30.27,0L90.11,81.23,31.08,86a16.46,16.46,0,0,0-9.37,28.86l45,38.83L53,211.75a16.38,16.38,0,0,0,24.5,17.82L128,198.49l50.53,31.08A16.4,16.4,0,0,0,203,211.75l-13.76-58.07,45-38.83A16.43,16.43,0,0,0,239.18,97.26Zm-15.34,5.47-48.7,42a8,8,0,0,0-2.56,7.91l14.88,62.8a.37.37,0,0,1-.17.48c-.18.14-.23.11-.38,0l-54.72-33.65a8,8,0,0,0-8.38,0L69.09,215.94c-.15.09-.19.12-.38,0a.37.37,0,0,1-.17-.48l14.88-62.8a8,8,0,0,0-2.56-7.91l-48.7-42c-.12-.1-.23-.19-.13-.5s.18-.27.33-.29l63.92-5.16A8,8,0,0,0,103,91.86l24.62-59.61c.08-.17.11-.25.35-.25s.27.08.35.25L153,91.86a8,8,0,0,0,6.75,4.92l63.92,5.16c.15,0,.24,0,.33.29S224,102.63,223.84,102.73Z",
            Icon::StarOn => "M234.29,114.85l-45,38.83L203,211.75a16.4,16.4,0,0,1-24.5,17.82L128,198.49,77.47,229.57A16.4,16.4,0,0,1,53,211.75l13.76-58.07-45-38.83A16.46,16.46,0,0,1,31.08,86l59-4.76,22.76-55.08a16.36,16.36,0,0,1,30.27,0l22.75,55.08,59,4.76a16.46,16.46,0,0,1,9.37,28.86Z",
            Icon::Remove => "M208.49,191.51a12,12,0,0,1-17,17L128,145,64.49,208.49a12,12,0,0,1-17-17L111,128,47.51,64.49a12,12,0,0,1,17-17L128,111l63.51-63.52a12,12,0,0,1,17,17L145,128Z",
        }
    }
}

type P = (f32, f32);

/// 경로 한 조각, 좌표는 모두 절대
#[derive(PartialEq, Debug)]
enum Seg {
    Move(P),
    Line(P),
    /// 조절점 둘과 끝점
    Cubic(P, P, P),
    /// 조절점과 끝점
    Quad(P, P),
    /// SVG 호 그대로: 반지름 (가로, 세로), 기울기 (도), 큰 호, 시계 방향
    Arc {
        to: P,
        radius: P,
        rotation: f32,
        large: bool,
        clockwise: bool,
    },
    Close,
}

/// 경로 글자를 읽는 자리
struct Scanner<'a> {
    s: &'a [u8],
    i: usize,
}

impl Scanner<'_> {
    fn skip(&mut self) {
        while matches!(self.s.get(self.i), Some(b' ' | b',' | b'\t' | b'\r' | b'\n')) {
            self.i += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip();
        self.s.get(self.i).copied()
    }

    /// 숫자 하나, 부호나 두 번째 소수점에서 다음 숫자로 넘어감 ("1-2", ".5.5")
    fn number(&mut self) -> Option<f32> {
        self.skip();
        let start = self.i;
        if matches!(self.s.get(self.i), Some(b'+' | b'-')) {
            self.i += 1;
        }
        let digits = |sc: &mut Self| {
            let from = sc.i;
            while sc.s.get(sc.i).is_some_and(u8::is_ascii_digit) {
                sc.i += 1;
            }
            sc.i > from
        };
        let int = digits(self);
        let frac = self.s.get(self.i) == Some(&b'.') && {
            self.i += 1;
            digits(self)
        };
        if !int && !frac {
            return None;
        }
        std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok()
    }

    /// 호의 플래그, 띄어 쓰지 않은 "01"도 한 글자씩
    fn flag(&mut self) -> Option<bool> {
        let f = match self.peek()? {
            b'0' => false,
            b'1' => true,
            _ => return None,
        };
        self.i += 1;
        Some(f)
    }

    /// 좌표 한 쌍, `o`를 더함 (상대 명령이면 지금 점)
    fn point(&mut self, o: P) -> Option<P> {
        Some((self.number()? + o.0, self.number()? + o.1))
    }
}

/// SVG 경로 데이터 (M L H V C S Q A Z, 절대·상대)를 절대 좌표 조각으로, 못 읽으면 None
fn parse(d: &str) -> Option<Vec<Seg>> {
    let mut sc = Scanner { s: d.as_bytes(), i: 0 };
    let mut out = Vec::new();
    let (mut cur, mut start) = ((0.0, 0.0), (0.0, 0.0));
    // 직전 3차 곡선의 두 번째 조절점, S가 지금 점 너머로 뒤집어 씀
    let mut last_ctrl: Option<P> = None;
    let mut cmd = 0u8;
    while let Some(b) = sc.peek() {
        if b.is_ascii_alphabetic() {
            sc.i += 1;
            cmd = b;
        } else if cmd == 0 || cmd.eq_ignore_ascii_case(&b'z') {
            return None;
        }
        let o = if cmd.is_ascii_lowercase() { cur } else { (0.0, 0.0) };
        let mut ctrl = None;
        match cmd.to_ascii_uppercase() {
            b'M' => {
                cur = sc.point(o)?;
                start = cur;
                out.push(Seg::Move(cur));
                // 이어지는 좌표는 선
                cmd = if cmd == b'm' { b'l' } else { b'L' };
            }
            b'L' => {
                cur = sc.point(o)?;
                out.push(Seg::Line(cur));
            }
            b'H' => {
                cur.0 = sc.number()? + o.0;
                out.push(Seg::Line(cur));
            }
            b'V' => {
                cur.1 = sc.number()? + o.1;
                out.push(Seg::Line(cur));
            }
            b'C' => {
                let (a, b, p) = (sc.point(o)?, sc.point(o)?, sc.point(o)?);
                out.push(Seg::Cubic(a, b, p));
                (ctrl, cur) = (Some(b), p);
            }
            b'S' => {
                let a = last_ctrl.map_or(cur, |c| (2.0 * cur.0 - c.0, 2.0 * cur.1 - c.1));
                let (b, p) = (sc.point(o)?, sc.point(o)?);
                out.push(Seg::Cubic(a, b, p));
                (ctrl, cur) = (Some(b), p);
            }
            b'Q' => {
                let (a, p) = (sc.point(o)?, sc.point(o)?);
                out.push(Seg::Quad(a, p));
                cur = p;
            }
            b'A' => {
                let radius = (sc.number()?, sc.number()?);
                let rotation = sc.number()?;
                let (large, clockwise) = (sc.flag()?, sc.flag()?);
                cur = sc.point(o)?;
                out.push(Seg::Arc { to: cur, radius, rotation, large, clockwise });
            }
            b'Z' => {
                out.push(Seg::Close);
                cur = start;
            }
            _ => return None,
        }
        last_ctrl = ctrl;
    }
    Some(out)
}

/// 아이콘 경로를 Direct2D 경로로, 채움 규칙은 SVG 기본 (nonzero)
pub fn geometry(factory: &ID2D1Factory1, icon: Icon) -> Result<ID2D1PathGeometry1> {
    let segs = parse(icon.path()).ok_or(Error::from(E_INVALIDARG))?;
    let v = |p: P| windows_numerics::Vector2 { X: p.0, Y: p.1 };
    unsafe {
        let path = factory.CreatePathGeometry()?;
        let sink = path.Open()?;
        sink.SetFillMode(D2D1_FILL_MODE_WINDING);
        let mut open = false;
        for seg in segs {
            match seg {
                Seg::Move(p) => {
                    if open {
                        sink.EndFigure(D2D1_FIGURE_END_OPEN);
                    }
                    sink.BeginFigure(v(p), D2D1_FIGURE_BEGIN_FILLED);
                    open = true;
                }
                Seg::Line(p) => sink.AddLine(v(p)),
                Seg::Cubic(a, b, p) => sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: v(a), point2: v(b), point3: v(p) }),
                Seg::Quad(a, p) => sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT { point1: v(a), point2: v(p) }),
                Seg::Arc { to, radius, rotation, large, clockwise } => sink.AddArc(&D2D1_ARC_SEGMENT {
                    point: v(to),
                    size: D2D_SIZE_F { width: radius.0, height: radius.1 },
                    rotationAngle: rotation,
                    sweepDirection: if clockwise { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                    arcSize: if large { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                }),
                Seg::Close => {
                    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    open = false;
                }
            }
        }
        if open {
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
        }
        sink.Close()?;
        Ok(path)
    }
}
