use egui::{text::LayoutJob, text::TextFormat, Color32, FontId, FontFamily};

/// 将字节索引向下取整到最近的字符边界
fn floor_char_boundary(text: &str, idx: usize) -> usize {
    let idx = idx.min(text.len());
    let mut i = idx;
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// 测量从 start 起 count 个字符的渲染高度
pub fn measure_chars(
    ctx: &egui::Context,
    text: &str,
    start: usize,
    count: usize,
    font_size: f32,
    line_height_px: f32,
    max_width: f32,
    font_family: &FontFamily,
) -> f32 {
    // start 也可能是非字符边界字节索引（prev_boundary 二分搜索会传入任意字节索引），
    // 必须先取整，否则 &text[start..end] 会 panic（中文多字节字符）。
    let start = floor_char_boundary(text, start);
    let end = floor_char_boundary(text, (start + count).min(text.len()));
    let slice = &text[start..end];
    if slice.is_empty() {
        return 0.0;
    }
    let mut job = LayoutJob::default();
    job.wrap.max_width = max_width;
    job.wrap.break_anywhere = false;
        job.append(
        slice,
        0.0,
        TextFormat {
            font_id: FontId::new(font_size, font_family.clone()),
            color: Color32::BLACK,
            line_height: Some(line_height_px),
            ..Default::default()
        },
    );
    let galley = ctx.fonts(|f| f.layout_job(job));
    galley.size().y
}

/// 估算一页大约能容纳多少字符
pub fn estimate_chars_at(
    ctx: &egui::Context,
    text: &str,
    t: usize,
    font_size: f32,
    line_height_px: f32,
    max_width: f32,
    page_height: f32,
    font_family: &FontFamily,
) -> usize {
    let remaining = text.len().saturating_sub(t);
    if remaining == 0 {
        return 0;
    }
    let probe = 300.min(remaining);
    let h = measure_chars(ctx, text, t, probe, font_size, line_height_px, max_width, font_family);
    let lines = (h / line_height_px).max(1.0);
    let page_lines = (page_height / line_height_px).max(1.0);
    ((probe as f64 / lines as f64) * page_lines as f64)
        .ceil()
        .max(16.0) as usize
}

/// 从 start 起最多能容纳多少字符（高度不超过 page_height，保证无半行）
pub fn page_end_at(
    ctx: &egui::Context,
    text: &str,
    start: usize,
    font_size: f32,
    line_height_px: f32,
    max_width: f32,
    page_height: f32,
    font_family: &FontFamily,
) -> usize {
    let remaining = text.len().saturating_sub(start);
    if remaining == 0 {
        return 0;
    }

    // 估算一页字符数作为搜索上界初值
    let probe = 300.min(remaining);
    let probe_h = measure_chars(ctx, text, start, probe, font_size, line_height_px, max_width, font_family);
    let probe_lines = (probe_h / line_height_px).max(1.0);
    let page_lines = (page_height / line_height_px).max(1.0);
    let est = ((probe as f64 / probe_lines as f64) * page_lines as f64)
        .ceil()
        .max(16.0) as usize;
    let mut hi = remaining.min(est);

    // 扩张上界，直到超出页面高度或到达文末
    loop {
        let h = measure_chars(ctx, text, start, hi, font_size, line_height_px, max_width, font_family);
        if h > page_height {
            break;
        }
        if hi >= remaining {
            return remaining;
        }
        hi = remaining.min(hi * 2);
    }

    // 二分查找：最大 count 使高度不超过 page_height
    let mut lo = 1usize;
    if measure_chars(ctx, text, start, 1, font_size, line_height_px, max_width, font_family) > page_height {
        return 1;
    }
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        let h = measure_chars(ctx, text, start, mid, font_size, line_height_px, max_width, font_family);
        if h <= page_height {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    // 确保 start + lo 落在字符边界上
    let end = floor_char_boundary(text, start + lo);
    end - start
}

/// 找 t 之前最近的页边界（t 本身是一个页起始）
pub fn prev_boundary(
    ctx: &egui::Context,
    text: &str,
    t: usize,
    font_size: f32,
    line_height_px: f32,
    max_width: f32,
    page_height: f32,
    font_family: &FontFamily,
) -> usize {
    if t == 0 {
        return 0;
    }
    let g = |x: usize| -> usize {
        x + page_end_at(ctx, text, x, font_size, line_height_px, max_width, page_height, font_family)
    };
    let est = estimate_chars_at(ctx, text, t, font_size, line_height_px, max_width, page_height, font_family);
    let win = 400.max((est as f64 * 1.6).ceil() as usize + 120);
    let mut lo0 = t.saturating_sub(win);
    while lo0 > 0 && g(lo0) >= t {
        lo0 = lo0.saturating_sub(win);
    }
    if g(lo0) >= t {
        return 0;
    }
    let mut lo = lo0;
    let mut hi = t - 1;
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if g(mid) >= t {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    floor_char_boundary(text, hi)
}

/// 从 start 起最多能容纳多少字符使其不超过一行高度（即"一行的字符量"）
pub fn line_end_at(
    ctx: &egui::Context,
    text: &str,
    start: usize,
    font_size: f32,
    line_height_px: f32,
    max_width: f32,
    font_family: &FontFamily,
) -> usize {
    let remaining = text.len().saturating_sub(start);
    if remaining == 0 {
        return start;
    }
    let mut hi = remaining.min(2000);
    while hi < remaining
        && measure_chars(ctx, text, start, hi, font_size, line_height_px, max_width, font_family)
            <= line_height_px
    {
        hi = remaining.min(hi * 2);
    }
    if measure_chars(ctx, text, start, hi, font_size, line_height_px, max_width, font_family) <= line_height_px {
        return floor_char_boundary(text, start + hi);
    }
    let mut lo = 1usize;
    if measure_chars(ctx, text, start, 1, font_size, line_height_px, max_width, font_family) > line_height_px {
        return floor_char_boundary(text, start + 1);
    }
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if measure_chars(ctx, text, start, mid, font_size, line_height_px, max_width, font_family)
            <= line_height_px
        {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    floor_char_boundary(text, start + lo)
}
