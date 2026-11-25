//! 控制栏矢量图标：直接用图元绘制，避免依赖字体里的符号字形。

use eframe::egui::{self, Color32, CornerRadius, Pos2, Stroke};

/// 控制栏图标。
pub enum Icon {
    Folder,
    ChevronLeft,
    ChevronRight,
    ZoomIn,
    ZoomOut,
    Fit,
    Settings,
    TextScan,
    Edit,
    Annotate,
    Close,
}

/// 图标外框尺寸（点）。
pub const ICON_SIZE: f32 = 20.0;

impl Icon {
    /// 以 `center` 为中心绘制图标，`size` 为外框边长。
    pub fn paint(&self, painter: &egui::Painter, center: Pos2, size: f32, color: Color32) {
        match self {
            Icon::Folder => folder(painter, center, size, color),
            Icon::ChevronLeft => chevron(painter, center, size, color, -1.0),
            Icon::ChevronRight => chevron(painter, center, size, color, 1.0),
            Icon::ZoomIn => magnifier(painter, center, size, color, true),
            Icon::ZoomOut => magnifier(painter, center, size, color, false),
            Icon::Fit => fit(painter, center, size, color),
            Icon::Settings => settings(painter, center, size, color),
            Icon::TextScan => text_scan(painter, center, size, color),
            Icon::Edit => edit(painter, center, size, color),
            Icon::Annotate => annotate(painter, center, size, color),
            Icon::Close => close(painter, center, size, color),
        }
    }
}

/// 图标按钮：只画图标，文字放到悬浮提示里。
pub fn button(
    ui: &mut egui::Ui,
    icon: Icon,
    size: f32,
    color: Color32,
    enabled: bool,
) -> egui::Response {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    // 点击区域比图标略大，便于点按。
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size + 6.0, size + 6.0), sense);
    if ui.is_rect_visible(rect) {
        let radius = CornerRadius::same(4);
        if enabled && (response.hovered() || response.is_pointer_button_down_on()) {
            let tint = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 32);
            ui.painter().rect_filled(rect, radius, tint);
        }
        let color = if enabled { color } else { color.gamma_multiply(0.4) };
        icon.paint(ui.painter(), rect.center(), size, color);
    }
    response
}

fn stroke_of(size: f32) -> f32 {
    (size * 0.085).clamp(1.2, 3.0)
}

/// 文件夹：主体矩形加顶部标签。
fn folder(painter: &egui::Painter, center: Pos2, size: f32, color: Color32) {
    let s = size;
    let tab = egui::Rect::from_min_max(
        egui::pos2(center.x - 0.40 * s, center.y - 0.34 * s),
        egui::pos2(center.x - 0.06 * s, center.y - 0.14 * s),
    );
    let body = egui::Rect::from_min_max(
        egui::pos2(center.x - 0.40 * s, center.y - 0.20 * s),
        egui::pos2(center.x + 0.40 * s, center.y + 0.32 * s),
    );
    let radius = CornerRadius::same(3);
    painter.rect_filled(tab, radius, color);
    painter.rect_filled(body, radius, color);
}

/// 左右箭头。
fn chevron(painter: &egui::Painter, center: Pos2, size: f32, color: Color32, dir: f32) {
    let s = size;
    let tip = center.x + dir * 0.22 * s;
    let back = center.x - dir * 0.10 * s;
    let points = vec![
        egui::pos2(back, center.y - 0.30 * s),
        egui::pos2(tip, center.y),
        egui::pos2(back, center.y + 0.30 * s),
    ];
    painter.add(egui::Shape::line(
        points,
        Stroke::new(stroke_of(s), color),
    ));
}

/// 放大镜，中间带加号或减号。
fn magnifier(painter: &egui::Painter, center: Pos2, size: f32, color: Color32, plus: bool) {
    let s = size;
    let lens = egui::pos2(center.x - 0.08 * s, center.y - 0.08 * s);
    let radius = 0.28 * s;
    let w = stroke_of(s);
    painter.circle_stroke(lens, radius, Stroke::new(w, color));
    painter.add(egui::Shape::line_segment(
        [
            egui::pos2(lens.x + radius * 0.7, lens.y + radius * 0.7),
            egui::pos2(center.x + 0.34 * s, center.y + 0.34 * s),
        ],
        Stroke::new(w, color),
    ));
    let arm = 0.16 * s;
    painter.add(egui::Shape::line_segment(
        [
            egui::pos2(lens.x - arm, lens.y),
            egui::pos2(lens.x + arm, lens.y),
        ],
        Stroke::new(w, color),
    ));
    if plus {
        painter.add(egui::Shape::line_segment(
            [
                egui::pos2(lens.x, lens.y - arm),
                egui::pos2(lens.x, lens.y + arm),
            ],
            Stroke::new(w, color),
        ));
    }
}

/// 适应窗口：四角括号。
fn fit(painter: &egui::Painter, center: Pos2, size: f32, color: Color32) {
    let s = size;
    let w = stroke_of(s);
    let half = 0.32 * s;
    let arm = 0.16 * s;
    let (x0, x1) = (center.x - half, center.x + half);
    let (y0, y1) = (center.y - half, center.y + half);
    let corners = [
        (egui::pos2(x0, y0), egui::pos2(x0 + arm, y0), egui::pos2(x0, y0 + arm)),
        (egui::pos2(x1, y0), egui::pos2(x1 - arm, y0), egui::pos2(x1, y0 + arm)),
        (egui::pos2(x0, y1), egui::pos2(x0 + arm, y1), egui::pos2(x0, y1 - arm)),
        (egui::pos2(x1, y1), egui::pos2(x1 - arm, y1), egui::pos2(x1, y1 - arm)),
    ];
    for (corner, horizontal, vertical) in corners {
        painter.add(egui::Shape::line_segment(
            [corner, horizontal],
            Stroke::new(w, color),
        ));
        painter.add(egui::Shape::line_segment(
            [corner, vertical],
            Stroke::new(w, color),
        ));
    }
}

/// 设置：三条带滑块的横线。
fn settings(painter: &egui::Painter, center: Pos2, size: f32, color: Color32) {
    let s = size;
    let w = stroke_of(s);
    let knob = 0.09 * s;
    let rows = [(-0.28, -0.14), (0.0, 0.16), (0.28, -0.06)];
    for (dy, knob_dx) in rows {
        let y = center.y + dy * s;
        painter.add(egui::Shape::line_segment(
            [
                egui::pos2(center.x - 0.36 * s, y),
                egui::pos2(center.x + 0.36 * s, y),
            ],
            Stroke::new(w, color),
        ));
        painter.circle_filled(
            egui::pos2(center.x + knob_dx * s, y),
            knob,
            color,
        );
    }
}

/// 编辑：斜放的铅笔。
fn edit(painter: &egui::Painter, center: Pos2, size: f32, color: Color32) {
    let s = size;
    let w = stroke_of(s);
    // 笔身：从左下指向右上的矩形。
    let dir = egui::vec2(0.7071, -0.7071);
    let perp = egui::vec2(-dir.y, dir.x);
    let half_len = 0.30 * s;
    let half_w = 0.09 * s;
    let center = center + egui::vec2(0.04 * s, -0.04 * s);
    let tail = center - dir * half_len;
    let head = center + dir * half_len * 0.35;
    let corners = [
        tail + perp * half_w,
        head + perp * half_w,
        head - perp * half_w,
        tail - perp * half_w,
    ];
    painter.add(egui::Shape::convex_polygon(
        corners.to_vec(),
        Color32::TRANSPARENT,
        egui::Stroke::new(w, color),
    ));
    // 笔尖：两条短线收成一点。
    let tip = center + dir * half_len;
    painter.add(egui::Shape::line_segment(
        [head + perp * half_w, tip],
        egui::Stroke::new(w, color),
    ));
    painter.add(egui::Shape::line_segment(
        [head - perp * half_w, tip],
        egui::Stroke::new(w, color),
    ));
}

/// 注释：圆角画框内一支指向右上角的箭头。
fn annotate(painter: &egui::Painter, center: Pos2, size: f32, color: Color32) {
    let s = size;
    let w = stroke_of(s);
    // 画框：居中的圆角矩形轮廓。
    let half = 0.33 * s;
    let frame = egui::Rect::from_min_max(
        egui::pos2(center.x - half, center.y - half),
        egui::pos2(center.x + half, center.y + half),
    );
    painter.add(egui::Shape::rect_stroke(
        frame,
        CornerRadius::from(0.08 * s),
        Stroke::new(w, color),
        egui::StrokeKind::Middle,
    ));
    // 箭头：左下指向右上，尖端两条短边收成箭头。
    let tail = egui::pos2(center.x - 0.16 * s, center.y + 0.16 * s);
    let tip = egui::pos2(center.x + 0.14 * s, center.y - 0.14 * s);
    painter.add(egui::Shape::line_segment([tail, tip], Stroke::new(w, color)));
    let leg = 0.10 * s;
    painter.add(egui::Shape::line_segment(
        [tip, egui::pos2(tip.x - leg, tip.y)],
        Stroke::new(w, color),
    ));
    painter.add(egui::Shape::line_segment(
        [tip, egui::pos2(tip.x, tip.y + leg)],
        Stroke::new(w, color),
    ));
}

/// 文字扫描：取景框四角加内部横线，表示识别文本。
fn text_scan(painter: &egui::Painter, center: Pos2, size: f32, color: Color32) {
    let s = size;
    let w = stroke_of(s);
    let half = 0.32 * s;
    let arm = 0.14 * s;
    let (x0, x1) = (center.x - half, center.x + half);
    let (y0, y1) = (center.y - half, center.y + half);
    let corners = [
        (egui::pos2(x0, y0), egui::pos2(x0 + arm, y0), egui::pos2(x0, y0 + arm)),
        (egui::pos2(x1, y0), egui::pos2(x1 - arm, y0), egui::pos2(x1, y0 + arm)),
        (egui::pos2(x0, y1), egui::pos2(x0 + arm, y1), egui::pos2(x0, y1 - arm)),
        (egui::pos2(x1, y1), egui::pos2(x1 - arm, y1), egui::pos2(x1, y1 - arm)),
    ];
    for (corner, horizontal, vertical) in corners {
        painter.add(egui::Shape::line_segment(
            [corner, horizontal],
            Stroke::new(w, color),
        ));
        painter.add(egui::Shape::line_segment(
            [corner, vertical],
            Stroke::new(w, color),
        ));
    }
    // 框内的两条文本线。
    for dy in [-0.10 * s, 0.12 * s] {
        painter.add(egui::Shape::line_segment(
            [
                egui::pos2(x0 + arm * 0.7, center.y + dy),
                egui::pos2(x1 - arm * 0.7, center.y + dy),
            ],
            Stroke::new(w * 0.8, color),
        ));
    }
}

/// 关闭：两条交叉短线。
fn close(painter: &egui::Painter, center: Pos2, size: f32, color: Color32) {
    let s = size;
    let w = stroke_of(s);
    let half = 0.22 * s;
    painter.add(egui::Shape::line_segment(
        [
            egui::pos2(center.x - half, center.y - half),
            egui::pos2(center.x + half, center.y + half),
        ],
        Stroke::new(w, color),
    ));
    painter.add(egui::Shape::line_segment(
        [
            egui::pos2(center.x + half, center.y - half),
            egui::pos2(center.x - half, center.y + half),
        ],
        Stroke::new(w, color),
    ));
}
