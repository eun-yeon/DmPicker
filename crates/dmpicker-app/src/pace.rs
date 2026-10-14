//! 인식 중 화살표 속도: 키를 누르고 있는 시간, 다음 누름 전까지 뗀 채로 같은 시간
//! 게임은 한 프레임에서 눌림, 다른 프레임에서 뗌을 봐야 입력으로 받음
//! 목록은 최대 35-40 ms마다 한 줄 움직임, 더 빨리 누르면 차례를 기다리고 일부는 멈춘 듯한 뒤에 움직임
//! 그래서 안전한 25 ms로 시작해, 첫 읽기에 누른 만큼 다 움직이는 동안 점점 빠르게 함
//! 그러지 못한 속도는 이 게임·PC에 너무 빠른 것이라 이번 인식에서 다시 안 씀

/// 누르는(그리고 떼는) 시간, 빠른 순
const PACE_MS: [u32; 6] = [8, 10, 13, 16, 20, SAFE_MS];
/// 시작값이자 덜 움직였을 때 내려가는 가장 느린 값
pub const SAFE_MS: u32 = 25;
/// 한 단계 빠르게 하기 전에 연속으로 다 움직여야 하는 횟수
const DOWN_AFTER: u32 = 3;

pub struct Pace {
    ms: u32,
    /// 연속으로 다 움직인 횟수
    run: u32,
    /// 누른 것보다 덜 움직인 가장 느린 속도 (0이면 없음)
    /// 그 속도와 더 빠른 속도는 다시 안 씀
    burnt: u32,
}

impl Pace {
    pub fn new() -> Self {
        Pace { ms: SAFE_MS, run: 0, burnt: 0 }
    }

    /// 한 번 누를 때 누르고 있는 시간, 뗀 뒤 기다리는 시간도 같음
    pub fn ms(&self) -> u32 {
        self.ms
    }

    /// `presses`번 누른 이동이 첫 읽기에 `first`줄, 다시 봤을 때 `shift`줄 움직임
    pub fn moved(&mut self, presses: usize, first: usize, shift: usize) {
        if shift == 0 {
            // 안 움직임, 프레임 레이트가 떨어졌거나 길게 끊김
            self.ms = (self.ms * 3 / 2).min(80);
            self.run = 0;
        } else if first < presses {
            // 놓쳤거나 늦은 입력, 한 단계 느리게 하되 25 ms보다 느려지진 않음
            // 25 ms와 그보다 느린 속도는 안 태움, 태우면 멈춤으로 느려진 뒤 25 ms로 못 돌아옴
            self.burnt = self.burnt.max(self.ms).min(SAFE_MS - 1);
            if let Some(&slower) = PACE_MS.iter().find(|&&p| p > self.ms) {
                self.ms = slower;
            }
            self.run = 0;
        } else {
            self.run += 1;
            let faster = PACE_MS.iter().rev().find(|&&p| p < self.ms).filter(|&&p| p > self.burnt);
            if let Some(&faster) = faster.filter(|_| self.run >= DOWN_AFTER) {
                self.ms = faster;
                self.run = 0;
            }
        }
    }
}
