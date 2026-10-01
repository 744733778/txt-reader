use regex::Regex;

#[derive(Debug, Clone)]
pub struct TocItem {
    pub title: String,
    pub offset: usize,
}

/// 章节识别结果
pub struct ChapterMatcher {
    strong: Vec<Regex>,
    special: Regex,
    weak: Vec<Regex>,
}

impl ChapterMatcher {
    pub fn new() -> Self {
        let num = r"[0-9零〇一二三四五六七八九十百千万两]+";
        Self {
            strong: vec![
                Regex::new(&format!(r"^【?第{}[章节回卷部集篇话]】?", num)).unwrap(),
                Regex::new(&format!(r"^{}[章节回卷部集篇话]", num)).unwrap(),
                Regex::new(r"^Chapter\s*[0-9IVXLCDMivxlcdm]+").unwrap(),
            ],
            special: Regex::new(
                r"^(楔子|序章|序言|前言|引子|自序|代序|尾声|后记|终章|番外|外传)([0-9零〇一二三四五六七八九十百千万两]*)$",
            )
            .unwrap(),
            weak: vec![
                Regex::new(&format!(r"^{}[、.．]\s*[^\s。！？]{{0,20}}$", num)).unwrap(),
                Regex::new(&format!(r"^{}{{1,4}}$", num)).unwrap(),
            ],
        }
    }

    /// 返回 2=强、1=弱、0=非章节
    fn match_chapter(&self, line: &str) -> u8 {
        let s: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        if s.is_empty() || s.len() > 60 {
            return 0;
        }
        for re in &self.strong {
            if re.is_match(&s) {
                return 2;
            }
        }
        if self.special.is_match(&s) {
            return 2;
        }
        for re in &self.weak {
            if re.is_match(&s) {
                return 1;
            }
        }
        0
    }

    /// 从全文提取目录
    pub fn build(&self, text: &str) -> Vec<TocItem> {
        let mut strong: Vec<TocItem> = Vec::new();
        let mut weak: Vec<TocItem> = Vec::new();

        let mut ls = 0usize;
        let n = text.len();

        // 逐行扫描（按 '\n' 分割）
        while ls < n {
            let nl = text[ls..].find('\n');
            let le = match nl {
                Some(p) => ls + p,
                None => n,
            };
            let line = &text[ls..le];
            let lv = self.match_chapter(line);
            if lv > 0 {
                let title: String = line
                    .trim_matches(|c: char| c.is_whitespace() || c == '\u{3000}')
                    .chars()
                    .map(|c| if c.is_whitespace() { ' ' } else { c })
                    .take(60)
                    .collect();
                let item = TocItem {
                    title,
                    offset: ls,
                };
                if lv == 2 {
                    strong.push(item);
                } else {
                    weak.push(item);
                }
            }
            if nl.is_none() {
                break;
            }
            ls = le + 1;
        }

        if strong.len() >= 3 {
            return strong;
        }

        // 强模式太少时，用弱模式兜底，丢弃与强候选相距过近的疑似正文行
        let mut keep: Vec<TocItem> = Vec::new();
        let strong_set: std::collections::HashSet<usize> =
            strong.iter().map(|s| s.offset).collect();
        for w in &weak {
            if strong_set.contains(&w.offset) {
                continue;
            }
            let mut near = false;
            for s in &strong {
                if (s.offset as isize - w.offset as isize).unsigned_abs() < 10 {
                    near = true;
                    break;
                }
            }
            if !near {
                keep.push(w.clone());
            }
        }

        let mut all = strong;
        all.extend(keep);
        all.sort_by_key(|a| a.offset);
        all
    }
}
