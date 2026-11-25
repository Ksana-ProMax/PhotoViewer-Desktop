//! 用户设置：背景色、预缓存张数、控制栏显隐、缩放步进与 OCR 选项。
//! 设置写入 eframe 的持久化存储，程序退出后自动恢复。

use eframe::egui::Color32;
use serde::{Deserialize, Serialize};

/// 持久化存储中保存设置的键名。
const STORAGE_KEY: &str = "settings";

/// 画布背景色。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Background {
    White,
    Black,
    Beige,
    LightGray,
}

impl Background {
    /// 全部可选背景色，顺序即设置界面中的顺序。
    pub const ALL: [Background; 4] = [
        Background::White,
        Background::Black,
        Background::Beige,
        Background::LightGray,
    ];

    /// 界面显示名。
    pub fn label(self) -> &'static str {
        match self {
            Background::White => "白色",
            Background::Black => "黑色",
            Background::Beige => "米色",
            Background::LightGray => "浅灰色",
        }
    }

    /// 背景色。
    pub fn color(self) -> Color32 {
        match self {
            Background::White => Color32::from_rgb(255, 255, 255),
            Background::Black => Color32::from_rgb(16, 16, 16),
            Background::Beige => Color32::from_rgb(240, 232, 214),
            Background::LightGray => Color32::from_rgb(211, 211, 211),
        }
    }

    /// 背景上的前景色，保证图标与文字可读。
    pub fn foreground(self) -> Color32 {
        match self {
            Background::Black => Color32::from_rgb(232, 232, 232),
            _ => Color32::from_rgb(48, 44, 40),
        }
    }

    /// 窗口内控件的常态底色：前景色的低透明度版本，叠在背景色上。
    pub fn widget_fill(self) -> Color32 {
        let fg = self.foreground();
        Color32::from_rgba_unmultiplied(fg.r(), fg.g(), fg.b(), 36)
    }

    /// 窗口内控件的悬停底色，比常态稍深。
    pub fn widget_fill_hover(self) -> Color32 {
        let fg = self.foreground();
        Color32::from_rgba_unmultiplied(fg.r(), fg.g(), fg.b(), 72)
    }
}

/// OCR 文本框的颜色。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum BoxColor {
    Blue,
    Red,
    Green,
    Black,
    White,
}

impl BoxColor {
    /// 全部可选颜色，顺序即设置界面中的顺序。
    pub const ALL: [BoxColor; 5] = [
        BoxColor::Blue,
        BoxColor::Red,
        BoxColor::Green,
        BoxColor::Black,
        BoxColor::White,
    ];

    /// 界面显示名。
    pub fn label(self) -> &'static str {
        match self {
            BoxColor::Blue => "蓝色",
            BoxColor::Red => "红色",
            BoxColor::Green => "绿色",
            BoxColor::Black => "黑色",
            BoxColor::White => "白色",
        }
    }

    /// 框的颜色。
    pub fn color(self) -> Color32 {
        match self {
            BoxColor::Blue => Color32::from_rgb(0, 120, 255),
            BoxColor::Red => Color32::from_rgb(220, 40, 40),
            BoxColor::Green => Color32::from_rgb(30, 160, 60),
            BoxColor::Black => Color32::from_rgb(0, 0, 0),
            BoxColor::White => Color32::from_rgb(255, 255, 255),
        }
    }
}

/// 应用设置。
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub background: Background,
    /// 前后各预缓存的图片张数。
    pub cache_count: usize,
    /// 是否自动隐藏底部控制栏。
    pub hide_controls: bool,
    /// 滚轮缩放一档的百分比。
    pub zoom_step: f32,
    /// OCR 文本框的颜色。
    pub ocr_box_color: BoxColor,
    /// OCR 结果缓存的图片张数上限。
    pub ocr_cache_cap: usize,
    /// 切换图片时是否自动保存未保存的编辑。
    #[serde(default)]
    pub auto_save_edit: bool,
    /// 切换图片时是否自动保存未保存的注释。
    #[serde(default)]
    pub auto_save_annotation: bool,
    /// 图片与窗口四边的间距（物理像素）。
    #[serde(default)]
    pub padding: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            background: Background::Beige,
            cache_count: 1,
            hide_controls: false,
            zoom_step: 10.0,
            ocr_box_color: BoxColor::Blue,
            ocr_cache_cap: crate::ocr_cache::DEFAULT_CACHE_CAP,
            auto_save_edit: false,
            auto_save_annotation: false,
            padding: 0,
        }
    }
}

impl Settings {
    /// 从持久化存储读取设置；没有存档或存档损坏时返回默认值。
    pub fn load(storage: Option<&dyn eframe::Storage>) -> Self {
        let Some(storage) = storage else {
            return Self::default();
        };
        if storage.get_string(STORAGE_KEY).is_none() {
            eprintln!("[settings] 首次运行，使用默认设置");
            return Self::default();
        }
        match eframe::get_value(storage, STORAGE_KEY) {
            Some(saved) => {
                eprintln!("[settings] 读取存档成功: {saved:?}");
                saved
            }
            None => {
                eprintln!("[settings] 存档解析失败，使用默认设置");
                Self::default()
            }
        }
    }

    /// 把设置写入持久化存储。
    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, STORAGE_KEY, self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::Storage as _;
    use std::collections::HashMap;

    /// 内存版存储，用于验证设置能真正写入并读回。
    struct MemoryStorage {
        kv: HashMap<String, String>,
    }

    impl eframe::Storage for MemoryStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.kv.get(key).cloned()
        }

        fn set_string(&mut self, key: &str, value: String) {
            self.kv.insert(key.to_owned(), value);
        }

        fn remove_string(&mut self, key: &str) {
            self.kv.remove(key);
        }

        fn flush(&mut self) {}
    }

    #[test]
    fn defaults_match_spec() {
        let s = Settings::default();
        assert_eq!(s.background, Background::Beige);
        assert_eq!(s.cache_count, 1);
        assert!(!s.hide_controls);
        assert_eq!(s.zoom_step, 10.0);
        assert_eq!(s.ocr_box_color, BoxColor::Blue);
        assert_eq!(s.ocr_cache_cap, 100);
        assert!(!s.auto_save_edit);
        assert!(!s.auto_save_annotation);
        assert_eq!(s.padding, 0);
    }

    /// 旧存档没有内边距字段，加载时按默认值补齐，不应整份设置作废。
    #[test]
    fn missing_padding_field_falls_back_to_default() {
        let mut storage = MemoryStorage {
            kv: HashMap::new(),
        };
        storage.set_string(
            STORAGE_KEY,
            "(background: Beige, cache_count: 1, hide_controls: false, zoom_step: 10.0, \
             ocr_box_color: Blue, ocr_cache_cap: 100, auto_save_edit: false)"
                .to_string(),
        );
        let loaded = Settings::load(Some(&storage));
        assert_eq!(loaded.padding, 0, "缺少内边距字段时应取默认值");
        assert_eq!(loaded.background, Background::Beige, "其余设置应正常读回");
    }

    #[test]
    fn settings_round_trip_through_storage() {
        let mut storage = MemoryStorage {
            kv: HashMap::new(),
        };
        let mut saved = Settings::default();
        saved.background = Background::Black;
        saved.cache_count = 7;
        saved.hide_controls = true;
        saved.zoom_step = 25.0;
        saved.ocr_box_color = BoxColor::Green;
        saved.ocr_cache_cap = 42;
        saved.auto_save_edit = true;
        saved.padding = 12;
        saved.save(&mut storage);
        storage.flush();

        let loaded = Settings::load(Some(&storage));
        assert_eq!(loaded, saved);
    }

    #[test]
    fn missing_storage_falls_back_to_defaults() {
        assert_eq!(Settings::load(None), Settings::default());
        let storage = MemoryStorage {
            kv: HashMap::new(),
        };
        assert_eq!(Settings::load(Some(&storage)), Settings::default());
    }

    #[test]
    fn box_color_palette_is_distinct() {
        let colors: Vec<Color32> = BoxColor::ALL.iter().map(|c| c.color()).collect();
        for (i, a) in colors.iter().enumerate() {
            for (j, b) in colors.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "颜色 {} 与 {} 不应相同", i, j);
                }
            }
        }
    }

    /// 背景色各有不同，且浅灰色已加入可选列表。
    #[test]
    fn background_palette_is_distinct() {
        let colors: Vec<Color32> = Background::ALL.iter().map(|b| b.color()).collect();
        for (i, a) in colors.iter().enumerate() {
            for (j, b) in colors.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "背景色 {} 与 {} 不应相同", i, j);
                }
            }
        }
        assert!(
            Background::ALL.contains(&Background::LightGray),
            "可选背景色应包含浅灰色"
        );
        // 深色底用浅字，浅色底用深字。
        assert!(Background::Black.foreground().g() > 200);
        assert!(Background::LightGray.foreground().g() < 100);
    }
}
