//! 注释：矩形、椭圆、直线、箭头四种形状的建模、绘制与保存。
//!
//! 形状用归一化坐标描述，预览与写盘共用 `geometry()`，保证所见即所得。

use eframe::egui;
use image::RgbaImage;

/// 椭圆的采样点数（四等分才能精确落在四个极值点上）。
const ELLIPSE_SAMPLES: usize = 64;

/// 注释形状。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    Rect,
    Ellipse,
    Line,
    Arrow,
}

impl Shape {
    /// 全部形状，顺序即界面上的顺序。
    pub const ALL: [Shape; 4] = [Shape::Rect, Shape::Ellipse, Shape::Line, Shape::Arrow];

    /// 界面显示名。
    pub fn label(self) -> &'static str {
        match self {
            Shape::Rect => "矩形",
            Shape::Ellipse => "椭圆形",
            Shape::Line => "直线",
            Shape::Arrow => "箭头",
        }
    }
}

/// 注释颜色。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Color {
    Blue,
    Red,
    Green,
    Yellow,
    Black,
    Purple,
}

impl Color {
    /// 全部可选颜色，顺序即界面上的顺序。
    pub const ALL: [Color; 6] = [
        Color::Blue,
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Black,
        Color::Purple,
    ];

    /// 界面显示名。
    pub fn label(self) -> &'static str {
        match self {
            Color::Blue => "蓝色",
            Color::Red => "红色",
            Color::Green => "绿色",
            Color::Yellow => "黄色",
            Color::Black => "黑色",
            Color::Purple => "紫色",
        }
    }

    /// 颜色值（不透明）。
    pub fn color(self) -> egui::Color32 {
        let [r, g, b] = self.rgb();
        egui::Color32::from_rgb(r, g, b)
    }

    /// RGB 分量：光栅化时直接使用，避免再从 Color32 反解。
    fn rgb(self) -> [u8; 3] {
        match self {
            Color::Blue => [0, 120, 255],
            Color::Red => [220, 40, 40],
            Color::Green => [30, 160, 60],
            Color::Yellow => [245, 200, 40],
            Color::Black => [0, 0, 0],
            Color::Purple => [150, 60, 220],
        }
    }
}

/// 一条注释：形状、颜色、粗细与两个端点。
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Annotation {
    /// 形状。
    pub shape: Shape,
    /// 颜色。
    pub color: Color,
    /// 粗细：占图片短边的百分比（0.1~10）。
    pub width_pct: f32,
    /// 起点：归一化坐标（0~1，原点在左上角，x 按宽、y 按高）。
    pub start: egui::Vec2,
    /// 终点：归一化坐标。
    pub end: egui::Vec2,
}

/// 注释几何：需要描边的折线与需要填充的多边形。
#[derive(Clone, Debug)]
pub struct Geometry {
    /// 描边折线，每个元素是一条折线。
    pub strokes: Vec<Vec<egui::Vec2>>,
    /// 填充多边形（箭头头部）。
    pub fills: Vec<Vec<egui::Vec2>>,
}

/// 粗细换算：百分比 → 目标坐标系的线宽（取目标短边）。
pub fn stroke_width(width_pct: f32, size: egui::Vec2) -> f32 {
    width_pct / 100.0 * size.x.min(size.y)
}

/// 计算形状在目标坐标系里的几何。size 为目标区域尺寸，width 与 size 同单位。
pub fn geometry(
    shape: Shape,
    start: egui::Vec2,
    end: egui::Vec2,
    size: egui::Vec2,
    width: f32,
) -> Geometry {
    let a = egui::Vec2::new(start.x * size.x, start.y * size.y);
    let b = egui::Vec2::new(end.x * size.x, end.y * size.y);
    let mut geo = Geometry {
        strokes: Vec::new(),
        fills: Vec::new(),
    };
    match shape {
        Shape::Rect => {
            let lo = a.min(b);
            let hi = a.max(b);
            // 首尾重复，直接按折线绘制即可闭合。
            geo.strokes.push(vec![
                lo,
                egui::Vec2::new(hi.x, lo.y),
                hi,
                egui::Vec2::new(lo.x, hi.y),
                lo,
            ]);
        }
        Shape::Ellipse => {
            let lo = a.min(b);
            let hi = a.max(b);
            let center = (lo + hi) / 2.0;
            let rx = (hi.x - lo.x) / 2.0;
            let ry = (hi.y - lo.y) / 2.0;
            let mut points = Vec::with_capacity(ELLIPSE_SAMPLES + 1);
            for i in 0..ELLIPSE_SAMPLES {
                let angle = i as f32 / ELLIPSE_SAMPLES as f32 * std::f32::consts::TAU;
                points.push(egui::Vec2::new(
                    center.x + rx * angle.cos(),
                    center.y + ry * angle.sin(),
                ));
            }
            points.push(points[0]);
            geo.strokes.push(points);
        }
        Shape::Line => {
            geo.strokes.push(vec![a, b]);
        }
        Shape::Arrow => {
            let delta = b - a;
            let rod = delta.length();
            // 杆身退化时给一个默认方向，箭头仍可见。
            let dir = if rod > 1e-6 {
                delta / rod
            } else {
                egui::Vec2::new(1.0, 0.0)
            };
            let half = (width * 1.5).max(2.0);
            let mut head = (width * 3.0).max(4.0);
            if rod > 1e-6 {
                head = head.min(rod);
            }
            let base = b - dir * head;
            let perp = egui::Vec2::new(-dir.y, dir.x);
            geo.strokes.push(vec![a, b]);
            geo.fills.push(vec![b, base + perp * half, base - perp * half]);
        }
    }
    geo
}

/// 把注释绘制到图片像素上（就地修改），用于写回原图。
pub fn draw_on_image(img: &mut RgbaImage, annotations: &[Annotation]) {
    if annotations.is_empty() || img.width() == 0 || img.height() == 0 {
        return;
    }
    let size = egui::Vec2::new(img.width() as f32, img.height() as f32);
    let mut segments = 0usize;
    for ann in annotations {
        let width = stroke_width(ann.width_pct, size);
        let geo = geometry(ann.shape, ann.start, ann.end, size, width);
        let rgb = ann.color.rgb();
        for poly in &geo.strokes {
            for pair in poly.windows(2) {
                draw_segment(img, pair[0], pair[1], width, rgb);
                segments += 1;
            }
        }
        for poly in &geo.fills {
            fill_polygon(img, poly, rgb);
        }
    }
    eprintln!("[anno] 已写入 {} 条注释（{segments} 段描边）", annotations.len());
}

/// 归一化坐标 → 图片矩形内的屏幕坐标。
pub fn screen_point(norm: egui::Vec2, rect: egui::Rect) -> egui::Pos2 {
    egui::pos2(
        rect.min.x + norm.x * rect.width(),
        rect.min.y + norm.y * rect.height(),
    )
}

/// 计算形状在画布屏幕坐标系下的几何：与 `geometry` 相同，但叠加画布左上角偏移。
///
/// 归一化坐标以图片左上角为原点，画布上的图片通常不在屏幕原点，
/// 因此屏幕坐标必须加上 `rect.min` 才是真正的落笔位置。
pub fn screen_geometry(
    shape: Shape,
    start: egui::Vec2,
    end: egui::Vec2,
    rect: egui::Rect,
    width: f32,
) -> Geometry {
    let size = egui::Vec2::new(rect.width(), rect.height());
    let mut geo = geometry(shape, start, end, size, width);
    let origin = rect.min.to_vec2();
    for poly in geo.strokes.iter_mut().chain(geo.fills.iter_mut()) {
        for point in poly.iter_mut() {
            *point += origin;
        }
    }
    geo
}

/// 把注释绘制到画布覆盖层（屏幕坐标）。
pub fn paint(painter: &egui::Painter, annotations: &[Annotation], rect: egui::Rect) {
    let size = egui::Vec2::new(rect.width(), rect.height());
    for ann in annotations {
        let width = stroke_width(ann.width_pct, size);
        let geo = screen_geometry(ann.shape, ann.start, ann.end, rect, width);
        let color = ann.color.color();
        for poly in &geo.strokes {
            painter.add(egui::Shape::line(
                poly.iter().map(|p| p.to_pos2()).collect(),
                egui::Stroke::new(width, color),
            ));
        }
        for poly in &geo.fills {
            painter.add(egui::Shape::convex_polygon(
                poly.iter().map(|p| p.to_pos2()).collect(),
                color,
                egui::Stroke::NONE,
            ));
        }
    }
}

/// 圆形笔刷绘制一段线：按到线段的距离算覆盖率，拐角与端点自然圆润。
fn draw_segment(img: &mut RgbaImage, p0: egui::Vec2, p1: egui::Vec2, width: f32, rgb: [u8; 3]) {
    let radius = (width * 0.5).max(0.5);
    let (w, h) = (img.width() as i32, img.height() as i32);
    let min_x = ((p0.x.min(p1.x) - radius).floor() as i32).clamp(0, w - 1);
    let max_x = ((p0.x.max(p1.x) + radius).ceil() as i32).clamp(0, w - 1);
    let min_y = ((p0.y.min(p1.y) - radius).floor() as i32).clamp(0, h - 1);
    let max_y = ((p0.y.max(p1.y) + radius).ceil() as i32).clamp(0, h - 1);
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let center = egui::Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            let dist = distance_to_segment(p0, p1, center);
            let cover = (radius + 0.5 - dist).clamp(0.0, 1.0);
            if cover > 0.0 {
                blend_pixel(img, x as u32, y as u32, rgb, cover);
            }
        }
    }
}

/// 扫描线填充多边形：内部按 0/1 覆盖填充，不做抗锯齿。
fn fill_polygon(img: &mut RgbaImage, poly: &[egui::Vec2], rgb: [u8; 3]) {
    if poly.len() < 3 {
        return;
    }
    let (w, h) = (img.width() as i32, img.height() as i32);
    let min_y = (poly.iter().fold(f32::INFINITY, |a, p| a.min(p.y)).floor() as i32).clamp(0, h - 1);
    let max_y = (poly.iter().fold(f32::NEG_INFINITY, |a, p| a.max(p.y)).ceil() as i32).clamp(0, h - 1);
    for y in min_y..=max_y {
        let scan = y as f32 + 0.5;
        let mut crossings: Vec<f32> = Vec::new();
        for i in 0..poly.len() {
            let a = poly[i];
            let b = poly[(i + 1) % poly.len()];
            if (a.y <= scan) != (b.y <= scan) {
                let t = (scan - a.y) / (b.y - a.y);
                crossings.push(a.x + t * (b.x - a.x));
            }
        }
        crossings.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        for pair in crossings.chunks(2) {
            let x0 = ((pair[0] - 0.5).ceil() as i32).clamp(0, w - 1);
            let x1 = ((pair[1] - 0.5).floor() as i32).clamp(0, w - 1);
            for x in x0..=x1 {
                blend_pixel(img, x as u32, y as u32, rgb, 1.0);
            }
        }
    }
}

/// 把不透明颜色按覆盖率混合到像素上。
fn blend_pixel(img: &mut RgbaImage, x: u32, y: u32, rgb: [u8; 3], cover: f32) {
    let dst = img.get_pixel(x, y).0;
    let dst_a = dst[3] as f32 / 255.0;
    let out_a = cover + dst_a * (1.0 - cover);
    if out_a <= 0.0 {
        return;
    }
    let mut out = [0u8; 4];
    for i in 0..3 {
        let src = rgb[i] as f32;
        let back = dst[i] as f32 * dst_a;
        let value = (src * cover + back * (1.0 - cover)) / out_a;
        out[i] = value.round().clamp(0.0, 255.0) as u8;
    }
    out[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
    img.put_pixel(x, y, image::Rgba(out));
}

/// 点到线段的距离。
fn distance_to_segment(p0: egui::Vec2, p1: egui::Vec2, p: egui::Vec2) -> f32 {
    let d = p1 - p0;
    let len_sq = d.length_sq();
    if len_sq <= f32::EPSILON {
        return (p - p0).length();
    }
    let t = ((p - p0).dot(d) / len_sq).clamp(0.0, 1.0);
    (p - (p0 + d * t)).length()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 纯白底图。
    fn solid(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_pixel(w, h, image::Rgba([255, 255, 255, 255]))
    }

    #[test]
    fn screen_geometry_shifts_by_canvas_origin() {
        // 图片不在屏幕原点时，屏幕几何必须带上画布左上角偏移。
        let rect = egui::Rect::from_min_max(
            egui::pos2(580.0, 25.0),
            egui::pos2(1325.0, 930.0),
        );
        let geo = screen_geometry(
            Shape::Rect,
            egui::Vec2::new(0.25, 0.5),
            egui::Vec2::new(0.75, 0.75),
            rect,
            2.0,
        );
        let pts = &geo.strokes[0];
        assert!(
            (pts[0] - egui::Vec2::new(580.0 + 0.25 * 745.0, 25.0 + 0.5 * 905.0)).length() < 1e-3,
            "屏幕起点应为画布原点加归一化坐标映射: {:?}",
            pts[0]
        );
        assert!(
            (pts[2] - egui::Vec2::new(580.0 + 0.75 * 745.0, 25.0 + 0.75 * 905.0)).length() < 1e-3,
            "屏幕终点应同步偏移: {:?}",
            pts[2]
        );
    }

    #[test]
    fn screen_geometry_keeps_relative_offsets_of_local_geometry() {
        let rect = egui::Rect::from_min_max(egui::pos2(100.0, 50.0), egui::pos2(500.0, 350.0));
        let size = egui::Vec2::new(rect.width(), rect.height());
        let local = geometry(
            Shape::Arrow,
            egui::Vec2::new(0.1, 0.2),
            egui::Vec2::new(0.9, 0.8),
            size,
            3.0,
        );
        let screen = screen_geometry(
            Shape::Arrow,
            egui::Vec2::new(0.1, 0.2),
            egui::Vec2::new(0.9, 0.8),
            rect,
            3.0,
        );
        let origin = rect.min.to_vec2();
        for (a, b) in local.strokes.iter().zip(screen.strokes.iter()) {
            for (pa, pb) in a.iter().zip(b.iter()) {
                assert!(((*pb - origin) - *pa).length() < 1e-3, "偏移应为纯平移");
            }
        }
        for (a, b) in local.fills.iter().zip(screen.fills.iter()) {
            for (pa, pb) in a.iter().zip(b.iter()) {
                assert!(((*pb - origin) - *pa).length() < 1e-3, "偏移应为纯平移");
            }
        }
    }

    #[test]
    fn rect_geometry_has_four_corners() {
        let geo = geometry(
            Shape::Rect,
            egui::Vec2::new(0.2, 0.3),
            egui::Vec2::new(0.8, 0.7),
            egui::Vec2::new(100.0, 100.0),
            2.0,
        );
        assert_eq!(geo.strokes.len(), 1);
        assert!(geo.fills.is_empty());
        let pts = &geo.strokes[0];
        assert_eq!(pts.len(), 5, "闭合折线应重复首点");
        assert!((pts[0] - egui::Vec2::new(20.0, 30.0)).length() < 1e-3);
        assert!((pts[1] - egui::Vec2::new(80.0, 30.0)).length() < 1e-3);
        assert!((pts[2] - egui::Vec2::new(80.0, 70.0)).length() < 1e-3);
        assert!((pts[3] - egui::Vec2::new(20.0, 70.0)).length() < 1e-3);
        assert!((pts[4] - pts[0]).length() < 1e-3);
    }

    #[test]
    fn rect_geometry_accepts_reversed_drag() {
        let geo = geometry(
            Shape::Rect,
            egui::Vec2::new(0.8, 0.7),
            egui::Vec2::new(0.2, 0.3),
            egui::Vec2::new(100.0, 100.0),
            2.0,
        );
        let pts = &geo.strokes[0];
        assert!((pts[0] - egui::Vec2::new(20.0, 30.0)).length() < 1e-3);
        assert!((pts[2] - egui::Vec2::new(80.0, 70.0)).length() < 1e-3);
    }

    #[test]
    fn ellipse_bounding_box_matches_outer_rect() {
        let geo = geometry(
            Shape::Ellipse,
            egui::Vec2::new(0.2, 0.3),
            egui::Vec2::new(0.8, 0.7),
            egui::Vec2::new(100.0, 100.0),
            2.0,
        );
        assert_eq!(geo.strokes.len(), 1);
        let pts = &geo.strokes[0];
        assert_eq!(pts.len(), ELLIPSE_SAMPLES + 1, "采样点加重复首点");
        let min_x = pts.iter().fold(f32::INFINITY, |a, p| a.min(p.x));
        let max_x = pts.iter().fold(f32::NEG_INFINITY, |a, p| a.max(p.x));
        let min_y = pts.iter().fold(f32::INFINITY, |a, p| a.min(p.y));
        let max_y = pts.iter().fold(f32::NEG_INFINITY, |a, p| a.max(p.y));
        assert!((min_x - 20.0).abs() < 0.01 && (max_x - 80.0).abs() < 0.01);
        assert!((min_y - 30.0).abs() < 0.01 && (max_y - 70.0).abs() < 0.01);
    }

    #[test]
    fn arrow_head_tip_points_to_end() {
        let geo = geometry(
            Shape::Arrow,
            egui::Vec2::new(0.0, 0.5),
            egui::Vec2::new(1.0, 0.5),
            egui::Vec2::new(100.0, 100.0),
            4.0,
        );
        assert_eq!(geo.strokes.len(), 1);
        assert_eq!(geo.fills.len(), 1);
        assert!((geo.strokes[0][0] - egui::Vec2::new(0.0, 50.0)).length() < 1e-3);
        assert!((geo.strokes[0][1] - egui::Vec2::new(100.0, 50.0)).length() < 1e-3);

        let head = &geo.fills[0];
        assert_eq!(head.len(), 3);
        assert!((head[0] - egui::Vec2::new(100.0, 50.0)).length() < 1e-3, "尖端在终点");
        assert!(head[1].x < head[0].x && head[2].x < head[0].x, "箭头底边在杆身后方");
        assert!((head[1].y - 50.0).abs() > 1.0 && (head[2].y - 50.0).abs() > 1.0);
        assert!((head[1].y + head[2].y - 100.0).abs() < 1e-3, "底边关于杆身对称");
    }

    #[test]
    fn arrow_head_length_never_exceeds_rod() {
        // 杆身短于箭头默认长度时按杆身长度收缩。
        let geo = geometry(
            Shape::Arrow,
            egui::Vec2::new(0.0, 0.0),
            egui::Vec2::new(0.01, 0.0),
            egui::Vec2::new(100.0, 100.0),
            10.0,
        );
        let head = &geo.fills[0];
        let rod = (geo.strokes[0][1] - geo.strokes[0][0]).length();
        let base_x = (head[1].x + head[2].x) / 2.0;
        assert!((head[0].x - base_x).abs() <= rod + 1e-3);
    }

    #[test]
    fn drawn_rect_colors_border_and_keeps_center() {
        let mut img = solid(100, 100);
        let ann = Annotation {
            shape: Shape::Rect,
            color: Color::Red,
            width_pct: 5.0,
            start: egui::Vec2::new(0.1, 0.1),
            end: egui::Vec2::new(0.9, 0.9),
        };
        draw_on_image(&mut img, &[ann]);

        assert_eq!(img.get_pixel(10, 10).0, [220, 40, 40, 255], "边角应为注释色");
        assert_eq!(img.get_pixel(50, 50).0, [255, 255, 255, 255], "中心应保持底色");
    }

    #[test]
    fn drawing_out_of_range_annotations_does_not_panic() {
        let mut img = solid(20, 20);
        let ann = Annotation {
            shape: Shape::Arrow,
            color: Color::Blue,
            width_pct: 10.0,
            start: egui::Vec2::new(-3.0, -3.0),
            end: egui::Vec2::new(4.0, 4.0),
        };
        draw_on_image(&mut img, &[ann]);
    }

    #[test]
    fn drawing_on_empty_image_does_not_panic() {
        let mut img = RgbaImage::new(0, 0);
        let ann = Annotation {
            shape: Shape::Rect,
            color: Color::Black,
            width_pct: 1.0,
            start: egui::Vec2::new(0.0, 0.0),
            end: egui::Vec2::new(1.0, 1.0),
        };
        draw_on_image(&mut img, &[ann]);
    }

    #[test]
    fn colors_are_all_distinct() {
        let mut seen: Vec<egui::Color32> = Vec::new();
        for color in Color::ALL {
            let value = color.color();
            assert!(!seen.contains(&value), "颜色重复: {}", color.label());
            seen.push(value);
        }
        assert_eq!(seen.len(), 6);
    }

    #[test]
    fn stroke_width_scales_by_short_side() {
        assert!((stroke_width(2.0, egui::Vec2::new(200.0, 100.0)) - 2.0).abs() < 1e-4);
        assert!((stroke_width(10.0, egui::Vec2::new(50.0, 50.0)) - 5.0).abs() < 1e-4);
        assert!((stroke_width(0.5, egui::Vec2::new(400.0, 200.0)) - 1.0).abs() < 1e-4);
    }
}
