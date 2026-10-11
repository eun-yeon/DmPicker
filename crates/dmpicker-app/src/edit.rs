//! 검색 입력창의 글 편집: 캐럿, 선택, 지우기, 단어 이동, 붙여넣기 정리
//! 위치는 바이트 단위지만 늘 화면 글자 (grapheme) 경계에 둠 (한글 음절, 결합 문자를 쪼개지 않음)
//! 단어는 공백으로 나눔, 글 전체는 화면 글자 `MAX_LETTERS`개까지

use unicode_segmentation::UnicodeSegmentation;

/// 검색어 최대 길이 (화면 글자), 큰 글을 잘못 붙여도 그리기가 느려지지 않게
pub const MAX_LETTERS: usize = 200;

#[derive(Default)]
pub struct Editor {
    text: String,
    caret: usize,
    /// 선택의 다른 쪽 끝, 캐럿과 같으면 선택 없음
    anchor: usize,
}

fn is_space(g: &str) -> bool {
    g.chars().all(char::is_whitespace)
}

/// 붙여넣을 글: 줄바꿈과 탭은 공백 하나로, 나머지 제어 문자는 뺌
pub fn clean_paste(s: &str) -> String {
    s.replace("\r\n", "\n").chars().filter_map(|c| if matches!(c, '\n' | '\r' | '\t') { Some(' ') } else { (!c.is_control()).then_some(c) }).collect()
}

impl Editor {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    /// 선택 범위 (앞, 뒤), 없으면 None
    pub fn selection(&self) -> Option<(usize, usize)> {
        (self.caret != self.anchor).then(|| (self.caret.min(self.anchor), self.caret.max(self.anchor)))
    }

    pub fn selected(&self) -> &str {
        self.selection().map_or("", |(a, b)| &self.text[a..b])
    }

    pub fn clear(&mut self) {
        *self = Editor::default();
    }

    fn prev(&self, i: usize) -> usize {
        self.text[..i].grapheme_indices(true).next_back().map_or(0, |(j, _)| j)
    }

    fn next(&self, i: usize) -> usize {
        self.text[i..].graphemes(true).next().map_or(i, |g| i + g.len())
    }

    /// 앞 단어의 처음: 공백을 건너뛰고 그 단어의 처음까지
    fn word_prev(&self, mut i: usize) -> usize {
        for space in [true, false] {
            while i > 0 && is_space(&self.text[self.prev(i)..i]) == space {
                i = self.prev(i);
            }
        }
        i
    }

    /// 다음 단어의 처음: 지금 단어를 지나 그 뒤 공백까지
    fn word_next(&self, mut i: usize) -> usize {
        for space in [false, true] {
            while i < self.text.len() && is_space(&self.text[i..self.next(i)]) == space {
                i = self.next(i);
            }
        }
        i
    }

    fn set(&mut self, to: usize, extend: bool) {
        self.caret = to;
        if !extend {
            self.anchor = to;
        }
    }

    /// `pos` (바이트)로 캐럿을 옮김, 화면 글자 사이가 아니면 그 앞 경계로, `extend`면 선택을 넓히거나 줄임
    pub fn move_to(&mut self, pos: usize, extend: bool) {
        let pos = pos.min(self.text.len());
        let at = self.text.grapheme_indices(true).map(|(i, _)| i).chain([self.text.len()]).take_while(|&i| i <= pos).last().unwrap_or(0);
        self.set(at, extend);
    }

    /// ← (`word`면 Ctrl+←), 선택이 있는데 그냥 ←면 선택의 앞으로
    pub fn left(&mut self, word: bool, extend: bool) {
        let to = match self.selection() {
            Some((a, _)) if !extend && !word => a,
            _ if word => self.word_prev(self.caret),
            _ => self.prev(self.caret),
        };
        self.set(to, extend);
    }

    /// → (`word`면 Ctrl+→), 선택이 있는데 그냥 →면 선택의 뒤로
    pub fn right(&mut self, word: bool, extend: bool) {
        let to = match self.selection() {
            Some((_, b)) if !extend && !word => b,
            _ if word => self.word_next(self.caret),
            _ => self.next(self.caret),
        };
        self.set(to, extend);
    }

    pub fn home(&mut self, extend: bool) {
        self.set(0, extend);
    }

    pub fn end(&mut self, extend: bool) {
        self.set(self.text.len(), extend);
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }

    /// 선택을 `s`로 바꾸고 (선택이 없으면 캐럿 자리에 넣고) 캐럿은 그 뒤로
    /// 남은 칸 (`MAX_LETTERS`)만큼만 넣음, 다 찼으면 선택만 지움
    pub fn insert(&mut self, s: &str) {
        let (a, b) = self.selection().unwrap_or((self.caret, self.caret));
        let kept = self.text[..a].graphemes(true).count() + self.text[b..].graphemes(true).count();
        let end = s.grapheme_indices(true).nth(MAX_LETTERS.saturating_sub(kept)).map_or(s.len(), |(i, _)| i);
        self.text.replace_range(a..b, &s[..end]);
        self.land(a + end);
    }

    fn remove(&mut self, a: usize, b: usize) -> bool {
        self.text.replace_range(a..b, "");
        self.land(a);
        a < b
    }

    /// 편집 뒤 캐럿을 `pos`에 둠, 앞뒤 글자가 한 화면 글자로 합쳐졌으면 (국기, 이모지 묶음) 그 뒤로
    fn land(&mut self, pos: usize) {
        let at = self.text.grapheme_indices(true).map(|(i, _)| i).chain([self.text.len()]).find(|&i| i >= pos).unwrap_or(pos);
        self.set(at, false);
    }

    /// 선택을 지움, 지운 게 있으면 true
    pub fn delete_selection(&mut self) -> bool {
        self.selection().is_some_and(|(a, b)| self.remove(a, b))
    }

    /// Backspace (`word`면 Ctrl+Backspace): 선택이 있으면 선택, 없으면 캐럿 앞 한 글자나 한 단어
    pub fn backspace(&mut self, word: bool) -> bool {
        if self.delete_selection() {
            return true;
        }
        let from = if word { self.word_prev(self.caret) } else { self.prev(self.caret) };
        self.remove(from, self.caret)
    }

    /// Delete (`word`면 Ctrl+Delete): 선택이 있으면 선택, 없으면 캐럿 뒤 한 글자나 한 단어
    pub fn delete(&mut self, word: bool) -> bool {
        if self.delete_selection() {
            return true;
        }
        let to = if word { self.word_next(self.caret) } else { self.next(self.caret) };
        self.remove(self.caret, to)
    }
}
