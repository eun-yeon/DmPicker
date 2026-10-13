//! packed RGB8 이미지 타입과 엔진에 필요한 필터 몇 개 (분리형 가우시안 블러, 안티앨리어싱 bilinear 축소)
//! 외부 이미지 처리 라이브러리는 안 씀

use std::path::Path;

/// 픽셀 단위 사각형 (`x`,`y` = 왼쪽 위, 오른쪽·아래 끝은 미포함)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Rect { x, y, w, h }
    }
    /// 오른쪽 끝 (미포함)
    pub const fn right(&self) -> i32 {
        self.x + self.w
    }
    /// 아래 끝 (미포함)
    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub const fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
    /// 교집합, 비면 `None`
    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
    }
}

/// packed RGB8 이미지 (행 우선, stride = `width * 3`)
#[derive(Clone, PartialEq, Eq)]
pub struct Rgb {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

impl std::fmt::Debug for Rgb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Rgb({}x{})", self.width, self.height)
    }
}

impl Rgb {
    /// 검은 이미지
    pub fn new(width: usize, height: usize) -> Self {
        Rgb {
            width,
            height,
            data: vec![0; width * height * 3],
        }
    }

    /// packed RGB 바이트를 그대로 감쌈, 길이가 안 맞으면 `None`
    pub fn from_raw(width: usize, height: usize, data: Vec<u8>) -> Option<Self> {
        (data.len() == width * height * 3).then_some(Rgb {
            width,
            height,
            data,
        })
    }

    /// 32비트 BGRA/BGRX 행에서 변환 (top-down 32bpp DIB section 형식)
    pub fn from_bgra(width: usize, height: usize, stride: usize, src: &[u8]) -> Self {
        let mut data = vec![0u8; width * height * 3];
        for (y, out) in data.chunks_exact_mut(width * 3).enumerate() {
            let row = &src[y * stride..y * stride + width * 4];
            for (o, i) in out.chunks_exact_mut(3).zip(row.chunks_exact(4)) {
                o[0] = i[2];
                o[1] = i[1];
                o[2] = i[0];
            }
        }
        Rgb {
            width,
            height,
            data,
        }
    }

    /// 지원하는 형식(PNG / WebP) 이미지 디코딩, 알파는 버림
    pub fn decode(bytes: &[u8]) -> Result<Self, image::ImageError> {
        Ok(Self::from_image(image::load_from_memory(bytes)?))
    }

    /// PNG / WebP 파일 읽기, 알파는 버림
    pub fn load(path: impl AsRef<Path>) -> Result<Self, image::ImageError> {
        Ok(Self::from_image(image::open(path)?))
    }

    fn from_image(im: image::DynamicImage) -> Self {
        let im = im.into_rgb8();
        let (w, h) = im.dimensions();
        Rgb {
            width: w as usize,
            height: h as usize,
            data: im.into_raw(),
        }
    }

    /// 이미지 전체 view
    pub fn view(&self) -> RgbView<'_> {
        RgbView {
            width: self.width,
            height: self.height,
            stride: self.width * 3,
            data: &self.data,
        }
    }

    /// 이미지 범위를 [`Rect`]로
    pub fn rect(&self) -> Rect {
        Rect::new(0, 0, self.width as i32, self.height as i32)
    }

    /// 이미지 안으로 잘라낸 부분 view (완전히 밖이면 빈 view)
    pub fn sub(&self, r: Rect) -> RgbView<'_> {
        self.view().sub(r)
    }

    /// 영역(이미지 안으로 잘라냄)을 복사한 이미지
    pub fn crop(&self, r: Rect) -> Rgb {
        self.sub(r).to_rgb()
    }

    #[inline]
    pub fn pixel(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * self.width + x) * 3;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }

    /// 안티앨리어싱 bilinear 크기 변경 (커널은 [`Op1d::blur_resize`]와 같음)
    pub fn resize(&self, w: usize, h: usize) -> Rgb {
        self.view().resize(w, h)
    }
}

/// 행 stride(바이트)를 따로 갖는 RGB8 view, `data`는 view 왼쪽 위 픽셀부터 시작
#[derive(Clone, Copy)]
pub struct RgbView<'a> {
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub data: &'a [u8],
}

impl<'a> RgbView<'a> {
    /// `y`번 행의 packed 픽셀 (`width * 3` 바이트)
    #[inline]
    pub fn row(&self, y: usize) -> &'a [u8] {
        &self.data[y * self.stride..y * self.stride + self.width * 3]
    }

    #[inline]
    pub fn pixel(&self, x: usize, y: usize) -> [u8; 3] {
        let i = y * self.stride + x * 3;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }

    /// 픽셀의 가장 밝은 채널 (HSV의 V)
    #[inline]
    pub fn value(&self, x: usize, y: usize) -> u8 {
        let p = self.pixel(x, y);
        p[0].max(p[1]).max(p[2])
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// 이 view 안으로 잘라낸 부분 view (완전히 밖이면 빈 view)
    pub fn sub(&self, r: Rect) -> RgbView<'a> {
        match r.intersect(&Rect::new(0, 0, self.width as i32, self.height as i32)) {
            Some(c) => {
                let off = c.y as usize * self.stride + c.x as usize * 3;
                RgbView {
                    width: c.w as usize,
                    height: c.h as usize,
                    stride: self.stride,
                    data: &self.data[off..],
                }
            }
            None => RgbView {
                width: 0,
                height: 0,
                stride: 0,
                data: &[],
            },
        }
    }

    /// packed 복사본
    pub fn to_rgb(&self) -> Rgb {
        let mut data = Vec::with_capacity(self.width * self.height * 3);
        for y in 0..self.height {
            data.extend_from_slice(self.row(y));
        }
        Rgb {
            width: self.width,
            height: self.height,
            data,
        }
    }

    /// 안티앨리어싱 bilinear 크기 변경 (커널은 [`Op1d::blur_resize`]와 같음)
    pub fn resize(&self, w: usize, h: usize) -> Rgb {
        let f = resample(*self, w, h, 0.0);
        Rgb {
            width: w,
            height: h,
            data: f.iter().map(|&x| to_u8(x)).collect(),
        }
    }

    /// 픽셀마다 `max(r,g,b) - min(r,g,b)`의 평균 (완전한 회색이면 0)
    pub fn mean_chroma(&self) -> f32 {
        if self.is_empty() {
            return 0.0;
        }
        let mut s = 0u64;
        for y in 0..self.height {
            for p in self.row(y).chunks_exact(3) {
                s += (p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2])) as u64;
            }
        }
        s as f32 / (self.width * self.height) as f32
    }
}

#[inline]
fn to_u8(x: f32) -> u8 {
    x.round().clamp(0.0, 255.0) as u8
}

/// 1차원 선형 연산 `out[j] = sum_i w[j][i] * in[start_j + i]` (희소 행)
#[derive(Clone, Debug)]
pub struct Op1d {
    pub in_len: usize,
    pub rows: Vec<(usize, Vec<f32>)>,
}

impl Op1d {
    /// 가우시안 블러(표준편차 `sigma`, 가장자리는 끝 값으로 채움) 뒤 `in_len`개 샘플을 `out_len`개로
    /// 안티앨리어싱 bilinear 리샘플 (축소 비율만큼 폭이 넓어지는 삼각 필터)
    /// `sigma <= 0`이면 블러 없음, `out_len == in_len`이면 리샘플 없음
    pub fn blur_resize(in_len: usize, out_len: usize, sigma: f32) -> Op1d {
        assert!(in_len > 0 && out_len > 0);
        let n = in_len as i64;
        // 가우시안 탭
        let (g, r) = if sigma > 0.05 {
            let r = (3.0 * sigma).ceil() as i64;
            let mut g: Vec<f64> = (-r..=r)
                .map(|t| (-(t * t) as f64 / (2.0 * (sigma as f64).powi(2))).exp())
                .collect();
            let s: f64 = g.iter().sum();
            g.iter_mut().for_each(|v| *v /= s);
            (g, r)
        } else {
            (vec![1.0], 0)
        };
        // bilinear 리샘플 계수
        let scale = in_len as f64 / out_len as f64;
        let fscale = scale.max(1.0);
        let support = fscale;
        let mut rows = Vec::with_capacity(out_len);
        let mut dense = vec![0f64; in_len];
        for j in 0..out_len {
            let center = (j as f64 + 0.5) * scale;
            let xmin = ((center - support + 0.5).floor() as i64).max(0);
            let xmax = ((center + support + 0.5).floor() as i64).min(n);
            let mut rw: Vec<(i64, f64)> = Vec::new();
            let mut tot = 0.0;
            for x in xmin..xmax {
                let t = ((x as f64 - center + 0.5) / fscale).abs();
                let w = if t < 1.0 { 1.0 - t } else { 0.0 };
                if w > 0.0 {
                    rw.push((x, w));
                    tot += w;
                }
            }
            dense.iter_mut().for_each(|v| *v = 0.0);
            for &(x, w) in &rw {
                let w = w / tot;
                for (ti, &gv) in g.iter().enumerate() {
                    let k = (x + ti as i64 - r).clamp(0, n - 1) as usize;
                    dense[k] += w * gv;
                }
            }
            let first = dense.iter().position(|&v| v > 1e-7).unwrap_or(0);
            let last = dense.iter().rposition(|&v| v > 1e-7).unwrap_or(0);
            rows.push((
                first,
                dense[first..=last].iter().map(|&v| v as f32).collect(),
            ));
        }
        Op1d { in_len, rows }
    }

    pub fn out_len(&self) -> usize {
        self.rows.len()
    }
}

/// view를 블러(표준편차 `sigma`, 0이면 없음)하고 `out_w x out_h`로 리샘플
/// 0..=255 범위 `f32` 값을 픽셀마다 RGB 순서로 이어서 돌려줌 (`out_h * out_w * 3`)
pub fn resample(v: RgbView, out_w: usize, out_h: usize, sigma: f32) -> Vec<f32> {
    let ox = Op1d::blur_resize(v.width, out_w, sigma);
    let oy = Op1d::blur_resize(v.height, out_h, sigma);
    apply_rgb(v, &ox, &oy)
}

/// RGB view에 분리형 연산 적용 (`ox`는 x 방향, `oy`는 y 방향)
pub fn apply_rgb(v: RgbView, ox: &Op1d, oy: &Op1d) -> Vec<f32> {
    debug_assert_eq!(ox.in_len, v.width);
    debug_assert_eq!(oy.in_len, v.height);
    let ow = ox.out_len();
    // 가로 처리, v.height x ow x 3
    let mut tmp = vec![0f32; v.height * ow * 3];
    for y in 0..v.height {
        let src = v.row(y);
        let dst = &mut tmp[y * ow * 3..(y + 1) * ow * 3];
        for (j, (start, w)) in ox.rows.iter().enumerate() {
            let (mut r, mut g, mut b) = (0f32, 0f32, 0f32);
            let s = &src[start * 3..(start + w.len()) * 3];
            for (wi, p) in w.iter().zip(s.chunks_exact(3)) {
                r += wi * p[0] as f32;
                g += wi * p[1] as f32;
                b += wi * p[2] as f32;
            }
            dst[j * 3] = r;
            dst[j * 3 + 1] = g;
            dst[j * 3 + 2] = b;
        }
    }
    apply_vertical(&tmp, ow * 3, oy)
}

/// 행 우선 `f32` 평면에 쓰는 [`apply_rgb`]의 단일 채널 버전
pub fn apply_plane(src: &[f32], w: usize, h: usize, ox: &Op1d, oy: &Op1d) -> Vec<f32> {
    debug_assert_eq!(src.len(), w * h);
    let ow = ox.out_len();
    let mut tmp = vec![0f32; h * ow];
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        for (j, (start, wt)) in ox.rows.iter().enumerate() {
            tmp[y * ow + j] = wt
                .iter()
                .zip(&row[*start..start + wt.len()])
                .map(|(a, b)| a * b)
                .sum();
        }
    }
    apply_vertical(&tmp, ow, oy)
}

/// 세로 처리, RGB와 단일 채널 평면이 같이 씀
fn apply_vertical(tmp: &[f32], row_len: usize, oy: &Op1d) -> Vec<f32> {
    let mut out = vec![0f32; oy.out_len() * row_len];
    for (i, (start, wt)) in oy.rows.iter().enumerate() {
        let dst = &mut out[i * row_len..(i + 1) * row_len];
        for (k, wi) in wt.iter().enumerate() {
            let s = &tmp[(start + k) * row_len..(start + k + 1) * row_len];
            for (d, v) in dst.iter_mut().zip(s) {
                *d += wi * v;
            }
        }
    }
    out
}
