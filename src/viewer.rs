//! 界面逻辑：底部控制栏切换同目录图片，画布支持缩放动画、边缘点击与相邻图片预取。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use eframe::egui;
use image::RgbaImage;

use crate::annotation;
use crate::editor::{self, EditState};
use crate::gallery;
use crate::geometry;
use crate::icons::{self, Icon};
use crate::loader::{self, Loader};
use crate::ocr::TextBox;
use crate::ocr_cache::OcrCache;
use crate::ocr_worker;
use crate::settings::{Background, BoxColor, Settings};

/// 目标尺寸稳定一段时间后才重建纹理，避免拖拽窗口时反复缩放。
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(120);
/// 有任务在途时的重绘间隔。
const POLL_INTERVAL: Duration = Duration::from_millis(60);
/// 缩放动画的时间常数（秒），越小收敛越快。
const ZOOM_TAU: f32 = 0.09;
/// 触控板滚动多少点算一格。
const POINTS_PER_NOTCH: f32 = 20.0;
/// 左右边缘点击换图的感应宽度（物理像素）。
const EDGE_PX: f32 = 10.0;
/// 鼠标靠近底边多少像素时唤出控制栏（物理像素）。
const BOTTOM_REVEAL_PX: f32 = 10.0;
/// 浮动窗口与窗口边缘的间距（点）。
const WINDOW_MARGIN: f32 = 16.0;
/// 关闭按钮的图标尺寸（点）。
const CLOSE_ICON_SIZE: f32 = 11.0;
/// 数值调节按钮的图标尺寸（点）。
const STEP_ICON_SIZE: f32 = 12.0;
/// 图片比视口大出多少比例后，缩放才完全跟随光标；中间为居中到光标的平滑过渡。
const BLEND_RATIO: f32 = 0.15;
/// 单帧耗时超过该毫秒数记为慢帧。
#[cfg(debug_assertions)]
const SLOW_FRAME_MS: f64 = 32.0;

/// 图片查看器：底部控制栏前后切换，画布按缩放比例显示图片。
pub struct PhotoViewer {
    loader: Loader,
    token: u64,
    gallery: Vec<PathBuf>,
    index: usize,
    source: Option<Arc<RgbaImage>>,
    tex: Option<egui::TextureHandle>,
    tex_px: [usize; 2],
    want_px: Option<[usize; 2]>,
    want_since: Option<Instant>,
    resizing: bool,
    loading: bool,
    /// 当前图片是否加载失败，失败时提示「图片加载失败」。
    load_failed: bool,
    status: Option<String>,
    settings: Settings,
    settings_open: bool,
    /// 设置自上次落盘后是否被改动。
    settings_dirty: bool,
    /// 当前显示比例（动画值，100 表示原始像素 1:1）。
    zoom: f32,
    /// 目标显示比例。
    zoom_target: f32,
    /// 缩放挡位的基准比例。
    zoom_base: f32,
    /// 是否已手动缩放，未缩放时始终按视口适配。
    zoom_manual: bool,
    /// 当前图片按视口适配的显示比例，作为手动缩放的挡位基准。
    fit_zoom: f32,
    /// 滚轮累积量。
    wheel: f32,
    /// 缩放锚点：光标相对视口中心的位置，以及其对应的图像空间点（原始像素）。
    zoom_anchor: Option<(egui::Vec2, egui::Vec2)>,
    /// 图片相对视口中心的偏移（点）。
    pan: egui::Vec2,
    /// 用户拖拽产生的偏移（点）。
    pan_drag: egui::Vec2,
    /// 画布视口中心（点）。
    view_center: egui::Vec2,
    /// 画布视口尺寸（点）。
    view_size: egui::Vec2,
    /// 每逻辑点对应的物理像素数。
    ppp: f32,
    /// 当前显示的图片尺寸（点）。
    view_image_size: egui::Vec2,
    /// 各轴图片是否超出视口，仅用于状态变化时打日志。
    fit_overflow: [bool; 2],
    /// OCR 后台工作线程。
    ocr: ocr_worker::OcrWorker,
    /// OCR 结果缓存（仅内存，退出即释放）。
    ocr_cache: OcrCache,
    /// 当前是否有识别任务在途。
    ocr_running: bool,
    /// 当前识别任务的进度（0.0~1.0）。
    ocr_progress: f32,
    /// 当前识别任务的阶段描述。
    ocr_stage: String,
    /// 识别任务令牌，用于丢弃过期结果。
    ocr_token: u64,
    /// 当前识别任务对应的图片路径，用于把结果写入正确的缓存项。
    ocr_path: Option<PathBuf>,
    /// 当前图片的识别结果。
    ocr_boxes: Vec<TextBox>,
    /// 选中的文本框序号。
    ocr_selected: Option<usize>,
    /// 文本弹窗是否打开。
    ocr_popup_open: bool,
    /// 当前图片在画布上的显示区域，用于坐标映射与命中检测。
    image_rect: egui::Rect,
    /// 当前显示图片的原始像素尺寸。
    image_size: [usize; 2],
    /// 显示内容的像素尺寸（编辑后的全分辨率结果），缩放与纹理以此为准。
    content_size: [usize; 2],
    /// 显示内容版本：每次换内容前进，用于丢弃过期纹理。
    content_version: u64,
    /// 纹理是否待更新：为真时保留旧纹理显示，直到新纹理就绪。
    texture_stale: bool,
    /// 编辑面板是否打开。
    edit_open: bool,
    /// 当前编辑参数。
    edit: EditState,
    /// 编辑后的预览图，未保存时用于显示。
    preview: Option<Arc<RgbaImage>>,
    /// 后台预览渲染线程。
    editor: editor::EditorWorker,
    /// 预览渲染令牌，用于丢弃过期结果。
    edit_token: u64,
    /// 当前原图的代号，换图时变化。
    source_id: u64,
    /// 底部控制栏的高度（点），浮动窗口据此避让。
    controls_height: f32,
    /// 已套用到全局样式的背景色，变化时才重新设置样式。
    styled_background: Option<Background>,
    /// 发起加载的时刻，用于量「点击打开 → 图片可用」的端到端耗时。
    load_started: Option<Instant>,
    /// 发起缩放的时刻，用于量「请求纹理 → 纹理上屏」的端到端耗时。
    resize_started: Option<Instant>,
    /// 发起识别的时刻，用于量「点击识别 → 结果返回」的端到端耗时。
    ocr_started: Option<Instant>,
    /// 发起预览渲染的时刻，用于量「改参数 → 预览就绪」的端到端耗时。
    preview_started: Option<Instant>,
    /// 已渲染的帧数，用于首帧与慢帧统计。
    frames: u64,
    /// 待打开的图片：目录扫描在后台线程完成，避免启动时卡住界面。
    pending_open: Option<PathBuf>,
    /// 目录扫描线程回传的结果：(图片列表, 目标图片下标)。
    open_rx: Option<Receiver<Result<(Vec<PathBuf>, usize), String>>>,
    /// 注释面板是否打开。
    anno_open: bool,
    /// 当前注释形状：Some 表示处于注释模式，等待一次绘制。
    anno_shape: Option<annotation::Shape>,
    /// 注释颜色。
    anno_color: annotation::Color,
    /// 注释粗细：占图片短边的百分比。
    anno_width: f32,
    /// 当前图片的注释列表。
    annotations: Vec<annotation::Annotation>,
    /// 正在拖拽的注释：起点与终点（归一化坐标）。
    anno_drag: Option<(egui::Vec2, egui::Vec2)>,
    /// 注释绘制日志的指纹：(条数, 图片矩形左上角)，变化时才打印，避免每帧刷屏。
    anno_log_sig: Option<(usize, [i32; 2])>,
    /// 待确认的换图请求：存在时换图被推迟，等用户点「确定」。
    pending_switch: Option<SwitchIntent>,
}

/// 按下位置所在的边缘区域。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Edge {
    None,
    Left,
    Right,
}

/// 待确认的换图请求：确认后才真正执行。
#[derive(Clone)]
enum SwitchIntent {
    /// 前后切换若干张。
    Step(i32),
    /// 打开指定图片。
    Open(PathBuf),
}

impl PhotoViewer {
    pub fn new() -> Self {
        perf_scope!("应用构造");
        Self {
            loader: Loader::spawn(),
            token: 0,
            gallery: Vec::new(),
            index: 0,
            source: None,
            tex: None,
            tex_px: [0, 0],
            want_px: None,
            want_since: None,
            resizing: false,
            loading: false,
            load_failed: false,
            status: None,
            settings: Settings::default(),
            settings_open: false,
            settings_dirty: false,
            zoom: 100.0,
            zoom_target: 100.0,
            zoom_base: 100.0,
            zoom_manual: false,
            fit_zoom: 100.0,
            wheel: 0.0,
            zoom_anchor: None,
            pan: egui::Vec2::ZERO,
            pan_drag: egui::Vec2::ZERO,
            view_center: egui::Vec2::ZERO,
            view_size: egui::Vec2::ZERO,
            view_image_size: egui::Vec2::ZERO,
            ppp: 1.0,
            fit_overflow: [false, false],
            ocr: ocr_worker::OcrWorker::spawn(ocr_worker::default_model_dir()),
            ocr_cache: OcrCache::new(crate::ocr_cache::DEFAULT_CACHE_CAP),
            ocr_running: false,
            ocr_progress: 0.0,
            ocr_stage: String::new(),
            ocr_token: 0,
            ocr_path: None,
            ocr_boxes: Vec::new(),
            ocr_selected: None,
            ocr_popup_open: false,
            image_rect: egui::Rect::ZERO,
            image_size: [0, 0],
            content_size: [0, 0],
            content_version: 0,
            texture_stale: false,
            edit_open: false,
            edit: EditState::default(),
            preview: None,
            editor: editor::EditorWorker::spawn(),
            edit_token: 0,
            source_id: 0,
            controls_height: 0.0,
            styled_background: None,
            load_started: None,
            resize_started: None,
            ocr_started: None,
            preview_started: None,
            frames: 0,
            pending_open: None,
            open_rx: None,
            anno_open: false,
            anno_shape: None,
            anno_color: annotation::Color::Blue,
            anno_width: 1.0,
            annotations: Vec::new(),
            anno_drag: None,
            anno_log_sig: None,
            pending_switch: None,
        }
    }

    /// 读取持久化设置，并把缓存容量等运行时状态同步到新设置。
    pub fn load_settings(&mut self, storage: Option<&dyn eframe::Storage>) {
        perf_scope!("读取设置");
        let saved = Settings::load(storage);
        if saved == self.settings {
            return;
        }
        eprintln!("[settings] 应用存档: {saved:?}");
        self.settings = saved;
        self.apply_cache_settings();
        self.ocr_cache.set_cap(self.settings.ocr_cache_cap);
    }

    /// 把当前设置写入持久化存储。
    pub fn persist_settings(&mut self, storage: &mut dyn eframe::Storage) {
        perf_scope!("写入设置");
        self.settings.save(storage);
        storage.flush();
        eprintln!("[settings] 已保存: {:?}", self.settings);
    }

    /// 打开指定图片，并把同目录图片作为前后切换的范围。
    ///
    /// 只登记路径就返回：目录扫描与解码都在后台完成，
    /// 界面先画出来，加载期间显示「正在加载」。
    pub fn open(&mut self, path: &Path) {
        perf_scope!("打开图片");
        // 识别进行中拒绝换图，避免结果与当前图片错位。
        if self.ocr_running {
            eprintln!("[open] 识别进行中，忽略打开请求");
            return;
        }
        // 换图前先处理未保存的注释：有注释时按设置自动保存或推迟等确认。
        if !self.prepare_switch(SwitchIntent::Open(path.to_path_buf())) {
            return;
        }
        // 换图前先处理未保存的编辑。
        self.finish_edits();
        // 登记待打开的图片，上一次未完成的扫描结果作废。
        self.pending_open = Some(path.to_path_buf());
        self.open_rx = None;
        // 立刻给反馈：画面清空并提示「正在加载」。
        self.clear_display();
        self.loading = true;
        self.load_failed = false;
        self.status = None;
        eprintln!("[open] 已登记待打开图片 {}", path.display());
    }

    /// 处理待打开的图片：目录扫描在后台线程，结果回来后再提交解码。
    fn pump_open(&mut self) {
        // 发起后台扫描。
        if let Some(path) = self.pending_open.take() {
            self.start_open_scan(path);
        }
        // 扫描还没结束就先等着，画面保持加载提示。
        let Some(rx) = &self.open_rx else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            // 扫描还在进行。
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            // 扫描线程异常退出：按打开失败处理，不让界面一直卡在加载中。
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err("目录扫描中断".to_string())
            }
        };
        self.open_rx = None;
        match result {
            Ok((gallery, index)) => {
                self.gallery = gallery;
                self.index = index;
                self.apply_cache_settings();
                // 视图与缩放由 load_current 统一复位到初始设定。
                self.load_current();
            }
            Err(err) => {
                eprintln!("[open] 打开失败: {err}");
                self.loading = false;
                self.load_failed = true;
                self.status = Some("图片加载失败".to_string());
            }
        }
    }

    /// 后台扫描目录，得到同目录图片列表与目标图片下标。
    fn start_open_scan(&mut self, path: PathBuf) {
        let (tx, rx) = channel();
        self.open_rx = Some(rx);
        std::thread::spawn(move || {
            // 文件不存在或不是普通文件，直接算打开失败。
            if !path.is_file() {
                let _ = tx.send(Err(format!("文件不存在: {}", path.display())));
                return;
            }
            let dir = path
                .parent()
                .filter(|dir| !dir.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let gallery = gallery::list_images(dir);
            let index = gallery.iter().position(|p| *p == path).unwrap_or(0);
            let _ = tx.send(Ok((gallery, index)));
        });
    }

    /// 清空当前画面：换图与加载期间只显示加载提示。
    fn clear_display(&mut self) {
        self.source = None;
        self.tex = None;
        self.tex_px = [0, 0];
        self.want_px = None;
        self.want_since = None;
        self.resizing = false;
        self.texture_stale = false;
        self.content_size = [0, 0];
    }

    /// 复位视图：缩放回到初始设定（适配视口），平移与锚点清零。
    fn reset_view(&mut self) {
        // 缩放不跨图片延续：新图按打开时的初始设定显示。
        if self.zoom_manual {
            eprintln!(
                "[zoom] 复位缩放到初始设定: {}% -> 适配视口",
                self.zoom_target
            );
        }
        self.zoom_manual = false;
        self.zoom = 100.0;
        self.zoom_target = 100.0;
        self.zoom_base = 100.0;
        self.zoom_anchor = None;
        self.pan = egui::Vec2::ZERO;
        self.pan_drag = egui::Vec2::ZERO;
    }

    /// 重新加载当前图片（解码在后台线程完成）。
    fn load_current(&mut self) {
        let Some(path) = self.gallery.get(self.index).cloned() else {
            return;
        };
        // 记下发起时刻，用于在解码回调里算端到端耗时。
        self.load_started = Some(Instant::now());
        self.token += 1;
        self.content_version += 1;
        self.clear_display();
        self.loading = true;
        self.load_failed = false;
        self.status = None;
        // 换图即回到打开时的初始设定，缩放不延续上一张。
        eprintln!("[zoom] 换图 {}: 缩放复位为初始设定", self.current_name());
        self.reset_view();
        // 换图后设置与编辑窗口自动收起，避免遮挡新图。
        self.close_panels();
        // 换图后编辑状态清空，预览不再适用。
        self.edit = EditState::default();
        self.preview = None;
        self.edit_token += 1;
        // 换图后注释清空：注释只属于当前图片。
        self.clear_annotations();
        self.restore_ocr_boxes();
        self.loader.load(self.token, self.content_version, path);
        self.prefetch_neighbors();
    }

    /// 换图后从缓存恢复该图片的识别结果；没有缓存则清空画框。
    fn restore_ocr_boxes(&mut self) {
        self.ocr_selected = None;
        self.ocr_popup_open = false;
        let path = self.gallery.get(self.index).cloned();
        self.ocr_boxes = match path {
            Some(path) => self
                .ocr_cache
                .get(&path)
                .cloned()
                .unwrap_or_default(),
            None => Vec::new(),
        };
    }

    /// 当前显示的图片：编辑预览优先，否则是原图。
    fn display_image(&self) -> Option<Arc<RgbaImage>> {
        self.preview.clone().or_else(|| self.source.clone())
    }

    /// 加载中的提示文字：失败优先提示失败，否则提示正在加载。
    fn loading_hint(&self) -> &'static str {
        if self.load_failed {
            "图片加载失败"
        } else {
            "正在加载…"
        }
    }

    /// 画布上没有图片时的提示文字。
    fn empty_hint(&self) -> &'static str {
        if self.load_failed {
            "图片加载失败"
        } else if self.loading {
            "正在加载…"
        } else {
            "拖放图片到窗口，或点击下方『打开图片』"
        }
    }

    /// 内容尺寸：编辑后的全分辨率结果优先，否则用显示图片的尺寸。
    fn content_of(&self, display: &RgbaImage) -> [usize; 2] {
        if self.content_size[0] == 0 || self.content_size[1] == 0 {
            [display.width() as usize, display.height() as usize]
        } else {
            self.content_size
        }
    }

    /// 更换显示内容并重建纹理。
    fn set_display(&mut self, image: Arc<RgbaImage>) {
        self.content_version += 1;
        self.mark_texture_stale();
        self.loader.replace(self.token, self.content_version, image);
    }

    /// 标记纹理待更新：保留旧纹理继续显示，新纹理就绪后无缝替换。
    fn mark_texture_stale(&mut self) {
        self.tex_px = [0, 0];
        self.texture_stale = true;
    }

    /// 按当前编辑参数提交预览渲染，结果由后台线程回传。
    fn refresh_preview(&mut self) {
        let Some(source) = self.source.clone() else {
            return;
        };
        // 令牌前进，在途的旧结果自动作废。
        self.edit_token += 1;
        // 参数回到默认时不再需要预览，直接显示原图。
        if !self.edit.is_modified() {
            self.preview = None;
            self.content_size = [source.width() as usize, source.height() as usize];
            self.set_display(source);
            return;
        }
        let (width, height) = self.edit.output_size(source.width(), source.height());
        self.content_size = [width as usize, height as usize];
        // 记下发起时刻，用于在预览回调里算端到端耗时。
        self.preview_started = Some(Instant::now());
        self.editor
            .render(self.edit_token, self.source_id, source, self.edit);
    }

    /// 收取后台渲染出的编辑预览。
    fn poll_editor(&mut self) {
        while let Ok(msg) = self.editor.msgs.try_recv() {
            match msg {
                editor::Msg::Rendered { token, image } => {
                    if token != self.edit_token {
                        continue;
                    }
                    eprintln!("[edit] 预览就绪 {}x{}", image.width(), image.height());
                    // 端到端：从改参数到预览就绪。
                    perf_since!(
                        self.preview_started,
                        "编辑预览就绪 {}x{}",
                        image.width(),
                        image.height()
                    );
                    self.preview = Some(image.clone());
                    self.set_display(image);
                }
            }
        }
    }

    /// 清空当前图片的识别结果：图片被编辑后旧坐标失效。
    fn clear_ocr(&mut self) {
        self.ocr_boxes.clear();
        self.ocr_selected = None;
        self.ocr_popup_open = false;
        if let Some(path) = self.gallery.get(self.index) {
            self.ocr_cache.remove(path);
        }
        // 在途的识别同样作废，否则旧坐标会画到新图上。
        if self.ocr_running {
            eprintln!("[edit] 编辑使进行中的识别作废");
            self.ocr_running = false;
            self.ocr_progress = 0.0;
            self.ocr_stage.clear();
            self.ocr_token += 1;
        }
    }

    /// 保存编辑结果并覆盖原图。
    fn save_edits(&mut self) {
        perf_scope!("保存编辑");
        let Some(path) = self.gallery.get(self.index).cloned() else {
            return;
        };
        let Some(source) = self.source.clone() else {
            return;
        };
        if !self.edit.is_modified() {
            self.status = Some("没有需要保存的修改".to_string());
            return;
        }
        let edited = self.edit.apply(source.as_ref());
        match editor::save_overwrite(&path, &edited) {
            Ok(()) => {
                eprintln!(
                    "[edit] 已覆盖保存 {} ({}x{})",
                    path.display(),
                    edited.width(),
                    edited.height()
                );
                let edited = Arc::new(edited);
                self.source = Some(edited.clone());
                self.preview = None;
                self.edit = EditState::default();
                self.source_id += 1;
                self.edit_token += 1;
                self.content_size = [edited.width() as usize, edited.height() as usize];
                // 原文件已改写，缓存里的旧图必须失效。
                self.loader.invalidate(&path);
                self.set_display(edited);
                self.clear_ocr();
                self.status = Some("已保存并覆盖原图".to_string());
            }
            Err(err) => {
                eprintln!("[edit] 保存失败: {err}");
                self.status = Some(err);
            }
        }
    }

    /// 放弃当前所有编辑，恢复原图。
    fn discard_edits(&mut self) {
        eprintln!("[edit] 放弃当前编辑");
        self.edit = EditState::default();
        self.preview = None;
        self.edit_token += 1;
        if let Some(source) = self.source.clone() {
            self.content_size = [source.width() as usize, source.height() as usize];
            self.set_display(source);
        }
        self.clear_ocr();
    }

    /// 换图前处理未保存的编辑：按设置自动保存或放弃。
    fn finish_edits(&mut self) {
        if !self.edit.is_modified() {
            return;
        }
        if self.settings.auto_save_edit {
            eprintln!("[edit] 自动保存开启，换图前保存");
            self.save_edits();
        } else {
            self.discard_edits();
        }
    }

    /// 收起浮动窗口：设置、编辑与注释面板在换图时关闭。
    fn close_panels(&mut self) {
        if self.settings_open || self.edit_open || self.anno_open {
            eprintln!("[ui] 换图，自动关闭设置、编辑与注释窗口");
        }
        self.settings_open = false;
        self.edit_open = false;
        self.anno_open = false;
        self.anno_shape = None;
    }

    /// 清空当前图片的注释，并退出注释模式。
    fn clear_annotations(&mut self) {
        if !self.annotations.is_empty() {
            eprintln!("[anno] 清空 {} 条注释", self.annotations.len());
        }
        self.annotations.clear();
        self.anno_shape = None;
        self.anno_drag = None;
        self.anno_log_sig = None;
    }

    /// 换图前的注释处理：无注释直接放行，开启自动保存则先保存，否则推迟等确认。
    ///
    /// 返回 false 表示换图被推迟，换图请求已记在 `pending_switch` 里。
    fn prepare_switch(&mut self, intent: SwitchIntent) -> bool {
        if self.annotations.is_empty() {
            return true;
        }
        if self.settings.auto_save_annotation {
            eprintln!("[anno] 自动保存开启，换图前保存注释");
            self.save_annotations();
            // 保存成功时注释列表已清空；失败时仍需用户确认。
            if self.annotations.is_empty() {
                return true;
            }
        }
        eprintln!("[anno] 存在未保存的注释，换图推迟等待确认");
        self.pending_switch = Some(intent);
        false
    }

    /// 保存注释并覆盖原图：底图取当前编辑结果，注释就地绘制后写盘。
    fn save_annotations(&mut self) {
        perf_scope!("保存注释");
        if self.annotations.is_empty() {
            self.status = Some("没有需要保存的注释".to_string());
            return;
        }
        let Some(path) = self.gallery.get(self.index).cloned() else {
            return;
        };
        let Some(source) = self.source.clone() else {
            return;
        };
        // 底图用编辑后的结果，保证写回文件的内容与屏幕一致。
        let mut composed = self.edit.apply(source.as_ref());
        if self.edit.is_modified() {
            eprintln!("[anno] 保存注释时一并写入未保存的编辑");
        }
        annotation::draw_on_image(&mut composed, &self.annotations);
        match editor::save_overwrite(&path, &composed) {
            Ok(()) => {
                eprintln!(
                    "[anno] 已覆盖保存 {} ({}x{})",
                    path.display(),
                    composed.width(),
                    composed.height()
                );
                let composed = Arc::new(composed);
                self.source = Some(composed.clone());
                self.preview = None;
                self.edit = EditState::default();
                self.source_id += 1;
                self.edit_token += 1;
                self.content_size = [composed.width() as usize, composed.height() as usize];
                // 原文件已改写，缓存里的旧图必须失效。
                self.loader.invalidate(&path);
                self.set_display(composed);
                self.clear_ocr();
                // 注释已经写进文件，列表清空。
                self.annotations.clear();
                self.anno_drag = None;
                self.status = Some("已保存注释并覆盖原图".to_string());
            }
            Err(err) => {
                eprintln!("[anno] 保存失败: {err}");
                self.status = Some(err);
            }
        }
    }

    /// 前后切换同目录图片：到首尾就停住，不循环。返回是否真的换了图。
    fn step(&mut self, delta: i32) -> bool {
        perf_scope!("切换图片");
        if self.gallery.len() < 2 {
            return false;
        }
        // 识别进行中禁止换图，避免结果与当前图片错位。
        if self.ocr_running {
            eprintln!("[switch] 识别进行中，忽略换图请求");
            return false;
        }
        let len = self.gallery.len() as i32;
        // 换图前先处理未保存的注释：有注释时按设置自动保存或推迟等确认。
        if !self.prepare_switch(SwitchIntent::Step(delta)) {
            return false;
        }
        // 换图前先处理未保存的编辑。
        self.finish_edits();
        // 首尾不循环：越界就停在边界上。
        let next = (self.index as i32 + delta).clamp(0, len - 1);
        if next == self.index as i32 {
            eprintln!(
                "[switch] 已在边界，停留于 {}/{} {}",
                self.index + 1,
                self.gallery.len(),
                self.current_name()
            );
            return false;
        }
        self.index = next as usize;
        eprintln!(
            "[switch] {}/{} {}",
            self.index + 1,
            self.gallery.len(),
            self.current_name()
        );
        self.load_current();
        true
    }

    /// 当前图片的文件名，用于日志与状态栏。
    fn current_name(&self) -> String {
        self.gallery
            .get(self.index)
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// 后台静默预取前后若干张图片，并写入缓存。
    fn prefetch_neighbors(&mut self) {
        let count = self.settings.cache_count;
        let len = self.gallery.len();
        if len == 0 || count == 0 {
            return;
        }
        self.loader.set_focus(self.index);
        let mut wanted: Vec<PathBuf> = Vec::new();
        // 列表不循环：前后邻居越界即停。
        for distance in 1..=count {
            let next = self.index.checked_add(distance).filter(|i| *i < len);
            let prev = self.index.checked_sub(distance);
            if next.is_none() && prev.is_none() {
                break;
            }
            if let Some(next) = next {
                wanted.push(self.gallery[next].clone());
            }
            if let Some(prev) = prev
                && Some(prev) != next
            {
                wanted.push(self.gallery[prev].clone());
            }
        }
        let queued = wanted.len();
        for path in wanted {
            self.loader.prefetch(self.index, path);
        }
        eprintln!(
            "[prefetch] queued {queued} images, cached {}",
            self.loader.cached_count()
        );
    }

    /// 把设置中的缓存张数同步到缓存容量。
    fn apply_cache_settings(&mut self) {
        self.loader.set_cache_cap(self.settings.cache_count * 2 + 3);
    }

    /// 收取后台线程的解码与缩放结果。
    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.loader.msgs.try_recv() {
            match msg {
                loader::Msg::Decoded { token, result } => {
                    if token != self.token {
                        continue;
                    }
                    self.loading = false;
                    match result {
                        Ok(rgba) => {
                            eprintln!(
                                "[decode] {} ({}x{})",
                                self.current_name(),
                                rgba.width(),
                                rgba.height()
                            );
                            // 端到端：从提交加载到图片可用于显示。
                            perf_since!(
                                self.load_started,
                                "图片可用 {} ({}x{})",
                                self.current_name(),
                                rgba.width(),
                                rgba.height()
                            );
                            self.source = Some(rgba);
                            if let Some(source) = &self.source {
                                self.content_size = [source.width() as usize, source.height() as usize];
                            }
                            // 原图换了，预览基准图也要跟着换。
                            self.source_id += 1;
                        }
                        Err(err) => {
                            // 失败原因写进日志，界面上只提示「图片加载失败」。
                            eprintln!("[load] 图片加载失败 {}: {err}", self.current_name());
                            self.source = None;
                            self.load_failed = true;
                            self.status = Some("图片加载失败".to_string());
                        }
                    }
                }
                loader::Msg::Resized {
                    token,
                    version,
                    px,
                    rgba,
                } => {
                    if token != self.token {
                        self.resizing = false;
                        continue;
                    }
                    self.resizing = false;
                    // 内容已经换过，这张纹理过期，丢弃即可。
                    if version != self.content_version {
                        continue;
                    }
                    let color = egui::ColorImage::from_rgba_unmultiplied(
                        [rgba.width() as usize, rgba.height() as usize],
                        rgba.as_raw(),
                    );
                    eprintln!("[texture] {} {}x{}", self.current_name(), px[0], px[1]);
                    // 端到端：从请求纹理到新纹理可用。
                    perf_since!(
                        self.resize_started,
                        "纹理就绪 {} {}x{}",
                        self.current_name(),
                        px[0],
                        px[1]
                    );
                    self.tex = Some(ctx.load_texture("photo", color, egui::TextureOptions::LINEAR));
                    self.tex_px = px;
                    self.texture_stale = false;
                }
            }
        }
    }

    /// 收取后台线程的 OCR 进度与结果。
    fn poll_ocr(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.ocr.msgs.try_recv() {
            match msg {
                ocr_worker::Msg::Progress {
                    token,
                    fraction,
                    label,
                } => {
                    if token != self.ocr_token {
                        continue;
                    }
                    self.ocr_progress = fraction;
                    self.ocr_stage = label;
                    ctx.request_repaint_after(POLL_INTERVAL);
                }
                ocr_worker::Msg::Done { token, result } => {
                    if token != self.ocr_token {
                        continue;
                    }
                    self.ocr_running = false;
                    self.ocr_progress = 0.0;
                    self.ocr_stage.clear();
                    // 结果按发起时的路径入缓存，不受当前索引影响。
                    let task_path = self.ocr_path.take();
                    match result {
                        Ok(boxes) => {
                            eprintln!(
                                "[ocr] {} 完成，{} 个文本区域",
                                self.current_name(),
                                boxes.len()
                            );
                            // 端到端：从点击识别到结果返回。
                            perf_since!(
                                self.ocr_started,
                                "识别完成 {}，{} 个区域",
                                self.current_name(),
                                boxes.len()
                            );
                            if let Some(path) = task_path.clone() {
                                // 编辑中的图片坐标基于预览尺寸，不入缓存。
                                if self.preview.is_none() {
                                    self.ocr_cache.put(path, boxes.clone());
                                }
                            }
                            // 只有任务对应的仍是当前图片时才上屏。
                            let current = self.gallery.get(self.index).cloned();
                            if current == task_path {
                                self.ocr_boxes = boxes;
                            }
                            self.ocr_selected = None;
                        }
                        Err(err) => {
                            eprintln!("[ocr] 失败: {err}");
                            self.status = Some(err);
                            self.ocr_boxes.clear();
                        }
                    }
                }
            }
        }
    }

    /// 触发当前图片的 OCR 识别。已有缓存时直接复用，不重复识别。
    fn run_ocr(&mut self) {
        if self.ocr_running {
            return;
        }
        let Some(path) = self.gallery.get(self.index).cloned() else {
            return;
        };
        // 编辑中的图片尺寸与原图不同，识别坐标对不上缓存，不走缓存。
        let cacheable = self.preview.is_none();
        // 缓存命中直接上屏，无需再次推理。
        if cacheable
            && let Some(boxes) = self.ocr_cache.get(&path).cloned()
        {
            eprintln!(
                "[ocr] 命中缓存 {} ({} 区域，缓存共 {} 张)",
                self.current_name(),
                boxes.len(),
                self.ocr_cache.len()
            );
            self.ocr_boxes = boxes;
            self.ocr_selected = None;
            self.ocr_popup_open = false;
            return;
        }
        let Some(display) = self.display_image() else {
            self.status = Some("图片尚未加载完成".to_string());
            return;
        };

        self.ocr_token += 1;
        self.ocr_running = true;
        self.ocr_progress = 0.0;
        self.ocr_stage = "准备中".to_string();
        self.ocr_path = Some(path.clone());
        self.ocr_boxes.clear();
        self.ocr_selected = None;
        self.ocr_popup_open = false;
        // 记下发起时刻，用于在结果返回时算端到端耗时。
        self.ocr_started = Some(Instant::now());
        eprintln!("[ocr] 开始识别 {}", self.current_name());
        self.ocr
            .recognize(self.ocr_token, path, display.as_ref().clone().into());
    }

    /// 确认纹理分辨率与视口匹配，必要时提交缩放任务。
    fn ensure_texture(&mut self, want: [usize; 2], immediate: bool) {
        if self.tex.is_some() && self.tex_px == want && !self.texture_stale {
            return;
        }
        if self.resizing {
            return;
        }
        let now = Instant::now();
        if self.want_px != Some(want) {
            self.want_px = Some(want);
            self.want_since = Some(now);
            // 手动缩放是离散操作，立即重建纹理；适配视口则等窗口稳定。
            if immediate {
                self.start_resize(want);
                return;
            }
        }
        let settled = self
            .want_since
            .is_some_and(|since| now.duration_since(since) >= RESIZE_DEBOUNCE);
        // 内容变了（编辑预览）时不等防抖，直接重建，避免画面停顿。
        if self.tex.is_none() || self.texture_stale || settled {
            self.start_resize(want);
        }
    }

    fn start_resize(&mut self, want: [usize; 2]) {
        self.resizing = true;
        // 记下发起时刻，用于在纹理回调里算端到端耗时。
        self.resize_started = Some(Instant::now());
        self.loader
            .resize(self.token, self.content_version, want);
    }

    /// 滚轮缩放：累计到一格就切换到下一挡，缩放中心跟随光标。
    fn handle_zoom_input(&mut self, ctx: &egui::Context) {
        if self.source.is_none() {
            return;
        }
        // 按滚轮格数累计，避免不同设备上滚动幅度差异影响手感。
        let notches = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|event| match event {
                    egui::Event::MouseWheel { unit, delta, .. } => match unit {
                        egui::MouseWheelUnit::Line => Some(delta.y),
                        egui::MouseWheelUnit::Point => Some(delta.y / POINTS_PER_NOTCH),
                        egui::MouseWheelUnit::Page => Some(delta.y * 10.0),
                    },
                    _ => None,
                })
                .sum::<f32>()
        });
        if notches == 0.0 {
            return;
        }
        self.wheel = (self.wheel + notches).clamp(-2.0, 2.0);
        let mouse = ctx.input(|i| i.pointer.hover_pos());
        if self.wheel >= 1.0 {
            self.wheel -= 1.0;
            self.zoom_step(1, mouse);
        } else if self.wheel <= -1.0 {
            self.wheel += 1.0;
            self.zoom_step(-1, mouse);
        }
        // 还有剩余滚动量时继续消费，避免快速滚动被吞掉。
        if self.wheel.abs() >= 1.0 {
            ctx.request_repaint();
        }
    }

    /// 切换到下一挡缩放比例，`mouse` 为缩放锚点（None 表示以视口中心为锚点）。
    fn zoom_step(&mut self, dir: i32, mouse: Option<egui::Pos2>) {
        if !self.zoom_manual {
            // 首次缩放把当前图片的适配比例作为挡位基准。
            self.zoom_base = self.fit_zoom;
            self.zoom_manual = true;
        }
        let next = geometry::zoom_gear(
            self.zoom_base,
            self.zoom_target,
            self.settings.zoom_step,
            dir,
        );
        eprintln!(
            "[zoom] {}% -> {}% (step {}%)",
            self.zoom_target,
            next,
            self.settings.zoom_step
        );
        self.zoom_target = next;
        self.set_zoom_anchor(mouse);
    }

    /// 记录缩放锚点：锚点对应的图像点在缩放过程中保持不动。
    fn set_zoom_anchor(&mut self, mouse: Option<egui::Pos2>) {
        let raw = match mouse {
            Some(pos) => (pos - self.view_center).to_vec2(),
            None => egui::Vec2::ZERO,
        };
        // 光标在图片之外时夹到图片范围内。
        let half = self.view_image_size * 0.5;
        let anchor = egui::vec2(
            raw.x.clamp(self.pan.x - half.x, self.pan.x + half.x),
            raw.y.clamp(self.pan.y - half.y, self.pan.y + half.y),
        );
        let img = (anchor - self.pan) * (100.0 / self.zoom) * self.ppp;
        self.zoom_anchor = Some((anchor, img));
        // 拖拽偏移并入新的锚点。
        self.pan_drag = egui::Vec2::ZERO;
    }

    /// 缩放锚点带来的偏移：按轴判断，未超出视口的轴固定居中，过渡区平滑混合。
    fn anchor_pan(&self) -> egui::Vec2 {
        let Some((cursor, img)) = self.zoom_anchor else {
            return egui::Vec2::ZERO;
        };
        let offset = img * (self.zoom / 100.0) / self.ppp;
        egui::vec2(
            axis_anchor(cursor.x, offset.x, self.view_image_size.x, self.view_size.x),
            axis_anchor(cursor.y, offset.y, self.view_image_size.y, self.view_size.y),
        )
    }

    /// 图片相对视口中心的偏移：锚点与拖拽合并。
    fn image_pan(&self) -> egui::Vec2 {
        self.anchor_pan() + self.pan_drag
    }

    /// 按当前比例更新图片的显示尺寸（点）。
    fn refresh_view_size(&mut self, content: Option<[usize; 2]>) {
        if let Some(content) = content {
            self.view_image_size = egui::vec2(
                content[0] as f32 * self.zoom / 100.0 / self.ppp,
                content[1] as f32 * self.zoom / 100.0 / self.ppp,
            );
        }
    }

    /// 未超出视口的轴不能保留拖拽偏移，否则图片会偏出视口。
    fn normalize_pan_drag(&mut self) {
        if self.view_image_size.x <= self.view_size.x {
            self.pan_drag.x = 0.0;
        }
        if self.view_image_size.y <= self.view_size.y {
            self.pan_drag.y = 0.0;
        }
    }

    /// 各轴是否超出视口的状态变化时打日志，便于确认缩放行为是否符合预期。
    fn log_fit_state(&mut self) {
        let overflow = [
            self.view_image_size.x > self.view_size.x,
            self.view_image_size.y > self.view_size.y,
        ];
        if overflow != self.fit_overflow {
            eprintln!(
                "[zoom] 超出视口 {:?} -> {:?}（显示 {:?}，视口 {:?}，居中系数 {:?}）",
                self.fit_overflow,
                overflow,
                self.view_image_size,
                self.view_size,
                [
                    axis_blend(self.view_image_size.x, self.view_size.x),
                    axis_blend(self.view_image_size.y, self.view_size.y),
                ]
            );
            self.fit_overflow = overflow;
        }
    }

    /// 缩放动画：指数逼近目标比例，避免瞬间跳变。
    fn animate_zoom(&mut self, ctx: &egui::Context) {
        if (self.zoom - self.zoom_target).abs() < 0.01 {
            self.zoom = self.zoom_target;
            return;
        }
        let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);
        let factor = 1.0 - (-dt / ZOOM_TAU).exp();
        self.zoom += (self.zoom_target - self.zoom) * factor;
        ctx.request_repaint();
    }

    /// 画布：按缩放比例显示图片，滚轮以光标为中心缩放，可拖动平移。
    fn canvas(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx();
        let ppp = ctx.pixels_per_point();
        let view = ui.max_rect();
        // 内边距：图片与窗口四边的距离。视口向内收缩后得到内容区，
        // 图片在内容区里居中和平移，拖到边缘时与窗口仍留出这段距离。
        let pad = (self.settings.padding as f32 / ppp)
            .min(view.width() * 0.25)
            .min(view.height() * 0.25);
        let area = egui::vec2(
            (view.width() - pad * 2.0).max(1.0),
            (view.height() - pad * 2.0).max(1.0),
        );
        let avail_px = [area.x * ppp, area.y * ppp];
        self.view_center = view.center().to_vec2();
        self.view_size = area;
        self.ppp = ppp;
        let response = ui.interact(
            view,
            ui.make_persistent_id("canvas"),
            egui::Sense::click_and_drag(),
        );

        // 显示内容与内容尺寸先算好：适配比例要赶在滚轮缩放之前得出，
        // 否则换图当帧的缩放会取到上一张图片的基准。
        let display = self.display_image();
        let content = display.as_ref().map(|image| self.content_of(image));
        let fit = content.map(|content| geometry::fit_scale_pct(content, avail_px));
        if let Some(value) = fit {
            self.fit_zoom = value;
        }
        // 显示尺寸先于滚轮处理更新：缩放锚点要据此判断图片是否超出视口。
        self.refresh_view_size(content);
        self.pan = self.image_pan();
        self.handle_zoom_input(ctx);

        let (Some(display), Some(content), Some(fit)) = (display, content, fit) else {
            ui.painter().text(
                view.center(),
                egui::Align2::CENTER_CENTER,
                self.empty_hint(),
                egui::FontId::proportional(16.0),
                self.settings.background.foreground(),
            );
            if self.loading {
                ctx.request_repaint_after(POLL_INTERVAL);
            }
            // 画面为空时只处理指针：这里没有内容尺寸，不会发纹理请求。
            let _ = self.handle_pointer(ctx, &response, view);
            return;
        };

        // 显示尺寸是真正画出来的像素，识别坐标以此为准。
        let display_size = [display.width() as usize, display.height() as usize];
        if self.zoom_manual {
            self.animate_zoom(ctx);
        } else {
            self.zoom = fit;
            self.zoom_target = fit;
            self.zoom_base = fit;
            self.zoom_anchor = None;
            self.pan_drag = egui::Vec2::ZERO;
        }

        // 当前显示的图片尺寸（点）。
        self.refresh_view_size(Some(content));
        self.normalize_pan_drag();
        self.pan = self.image_pan();
        self.log_fit_state();
        let switched = self.handle_pointer(ctx, &response, view);

        let max_side = ctx.input(|i| i.max_texture_side);
        let target = geometry::scale_px(content, self.zoom_target, max_side);
        let want = [
            target[0].round().max(1.0) as usize,
            target[1].round().max(1.0) as usize,
        ];
        // 换图后画面已清空，本帧的内容尺寸已作废：再按它请求纹理会多生成一张
        // 无人观看的全分辨率纹理，换图因此慢上半秒。改为等新图就绪后再请求。
        if switched {
            eprintln!(
                "[texture] 换图后跳过本帧纹理请求（原内容 {}x{}）",
                content[0], content[1]
            );
        } else {
            self.ensure_texture(want, self.zoom_manual);
        }

        let rect = egui::Rect::from_center_size(view.center() + self.pan, self.view_image_size);
        self.image_rect = rect;
        self.image_size = display_size;
        match &self.tex {
            Some(tex) => {
                ui.painter().image(
                    tex.id(),
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            None => {
                // 纹理还在生成：只提示加载状态，不再显示占位文案。
                ui.painter().text(
                    view.center(),
                    egui::Align2::CENTER_CENTER,
                    self.loading_hint(),
                    egui::FontId::proportional(16.0),
                    self.settings.background.foreground(),
                );
            }
        }
        self.draw_ocr_boxes(ui.painter(), rect, display_size);
        // 注释画在 OCR 框之上，避免被框线遮挡。
        self.handle_annotation_input(ctx, &response, rect);
        self.draw_annotations(ui.painter(), rect);
        if self.resizing || self.tex_px != want {
            ctx.request_repaint_after(POLL_INTERVAL);
        }
    }

    /// 在画布上绘制 OCR 文本框。坐标由原图像素映射到当前显示区域。
    fn draw_ocr_boxes(&self, painter: &egui::Painter, rect: egui::Rect, original: [usize; 2]) {
        if self.ocr_boxes.is_empty() || original[0] == 0 || original[1] == 0 {
            return;
        }
        let color = self.settings.ocr_box_color.color();
        let scale_x = rect.width() / original[0] as f32;
        let scale_y = rect.height() / original[1] as f32;
        let selected = self.ocr_selected;
        for (index, item) in self.ocr_boxes.iter().enumerate() {
            let points: Vec<egui::Pos2> = item
                .quad
                .iter()
                .map(|&(x, y)| egui::pos2(rect.min.x + x * scale_x, rect.min.y + y * scale_y))
                .collect();
            // 选中的框加粗，便于区分。
            let width = if Some(index) == selected { 2.5 } else { 1.5 };
            painter.add(egui::Shape::closed_line(
                points,
                egui::Stroke::new(width, color),
            ));
        }
    }

    /// 命中检测：找出鼠标位置所在的文本框序号。
    fn ocr_box_at(&self, pos: egui::Pos2, rect: egui::Rect, original: [usize; 2]) -> Option<usize> {
        if self.ocr_boxes.is_empty() || original[0] == 0 || original[1] == 0 {
            return None;
        }
        let scale_x = rect.width() / original[0] as f32;
        let scale_y = rect.height() / original[1] as f32;
        // 从后往前找，优先命中后绘制的（视觉上更靠上）。
        self.ocr_boxes.iter().enumerate().rev().find_map(|(index, item)| {
            let points: Vec<egui::Pos2> = item
                .quad
                .iter()
                .map(|&(x, y)| egui::pos2(rect.min.x + x * scale_x, rect.min.y + y * scale_y))
                .collect();
            if point_in_polygon(pos, &points) {
                Some(index)
            } else {
                None
            }
        })
    }

    /// 处理画布指针：拖动平移图片，点击文本框弹出文字，点击左右边缘换图。返回是否换了图。
    fn handle_pointer(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
        view: egui::Rect,
    ) -> bool {
        // 注释模式下一律跳过：不平移、不边缘换图、不弹 OCR 文字框。
        if self.anno_shape.is_some() {
            return false;
        }
        let ppp = ctx.pixels_per_point();
        // 拖动平移：只有图片超出视口的轴才能移动。
        if response.dragged() {
            self.apply_drag(response.drag_delta());
            self.pan = self.image_pan();
        }
        // 点击左右边缘换图；拖动过则不会触发 clicked。
        if response.clicked() {
            let pos = response
                .interact_pointer_pos()
                .or_else(|| ctx.input(|i| i.pointer.hover_pos()))
                .unwrap_or_else(|| view.center());
            // 先判断是否点中文本框，命中则弹出该框文字，不再换图。
            if let Some(index) = self.ocr_box_at(pos, self.image_rect, self.image_size) {
                self.ocr_selected = Some(index);
                self.ocr_popup_open = true;
                eprintln!("[ocr] 选中文本框 #{index}");
                return false;
            }
            self.ocr_selected = None;
            self.ocr_popup_open = false;
            return match edge_of(view, pos, ppp) {
                Edge::Left => self.step(-1),
                Edge::Right => self.step(1),
                Edge::None => false,
            };
        }
        false
    }

    /// 处理注释绘制：按下记起点，拖动时实时预览，松手落笔。
    ///
    /// 只有画布自身的按下才生效：点在浮动窗口上的鼠标不会误触发画布逻辑。
    fn handle_annotation_input(
        &mut self,
        ctx: &egui::Context,
        response: &egui::Response,
        rect: egui::Rect,
    ) {
        let Some(shape) = self.anno_shape else {
            return;
        };
        // 换图确认框弹出时不接收画布输入。
        if self.pending_switch.is_some() {
            return;
        }
        let released = ctx.input(|i| i.pointer.button_released(egui::PointerButton::Primary));
        // 拖动中：终点跟随鼠标并夹在图片区域内。
        if let Some((start, _)) = self.anno_drag {
            if let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) {
                let end = normalized_point(clamp_into_rect(pos, rect), rect);
                self.anno_drag = Some((start, end));
            }
            if released {
                self.commit_annotation(shape, rect);
            }
            return;
        }
        // 新的按下：按下与松开同帧时标记已被清除，用 clicked 兜底。
        if !response.is_pointer_button_down_on() && !response.clicked() {
            return;
        }
        let Some(pos) = response.interact_pointer_pos() else {
            return;
        };
        if !rect.contains(pos) {
            // 按在图片外面：直接结束注释模式，不画任何注释。
            eprintln!("[anno] 图片外按下，结束注释模式");
            self.anno_shape = None;
            return;
        }
        let start = normalized_point(pos, rect);
        self.anno_drag = Some((start, start));
        if released {
            self.commit_annotation(shape, rect);
        }
    }

    /// 落笔：把当前拖拽的图形加入注释列表，并结束注释模式。
    fn commit_annotation(&mut self, shape: annotation::Shape, rect: egui::Rect) {
        let Some((start, end)) = self.anno_drag.take() else {
            return;
        };
        let anno = annotation::Annotation {
            shape,
            color: self.anno_color,
            width_pct: self.anno_width,
            start,
            end,
        };
        eprintln!(
            "[anno] 添加注释: {:?} 颜色 {:?} 粗细 {}%，屏幕起点 {:?} 终点 {:?}",
            anno.shape,
            anno.color,
            anno.width_pct,
            annotation::screen_point(start, rect),
            annotation::screen_point(end, rect)
        );
        self.annotations.push(anno);
        // 一次只画一条，落笔后需要重新点形状按钮。
        self.anno_shape = None;
    }

    /// 在画布上叠加注释：拖拽中的临时图形一并画出，实现实时预览。
    fn draw_annotations(&mut self, painter: &egui::Painter, rect: egui::Rect) {
        if self.annotations.is_empty() && self.anno_drag.is_none() {
            return;
        }
        annotation::paint(painter, &self.annotations, rect);
        if let (Some(shape), Some((start, end))) = (self.anno_shape, self.anno_drag) {
            let preview = annotation::Annotation {
                shape,
                color: self.anno_color,
                width_pct: self.anno_width,
                start,
                end,
            };
            annotation::paint(painter, &[preview], rect);
        }
        // 落笔位置日志：打印归一化坐标映射回的屏幕点，
        // 与鼠标点击位置一致即说明没有错位（仅在条数或图片位置变化时打印）。
        let sig = (
            self.annotations.len(),
            [rect.min.x.round() as i32, rect.min.y.round() as i32],
        );
        if self.anno_log_sig != Some(sig) {
            self.anno_log_sig = Some(sig);
            let sample = self
                .annotations
                .first()
                .map(|ann| annotation::screen_point(ann.start, rect));
            eprintln!(
                "[anno] 叠加绘制 {} 条注释，图片矩形 min=({:.1},{:.1}) size=({:.1},{:.1})，首条起点屏幕坐标 {:?}",
                self.annotations.len(),
                rect.min.x,
                rect.min.y,
                rect.width(),
                rect.height(),
                sample
            );
        }
    }

    /// 应用拖拽位移：按图片超出视口的方向决定可拖动范围。
    ///
    /// 图片哪一侧露在视口外，就只能朝该方向拖到齐平为止；
    /// 图片未超出视口的轴直接回到视口中心。
    fn apply_drag(&mut self, delta: egui::Vec2) {
        let current = self.image_pan();
        let next = egui::vec2(
            axis_drag(
                current.x,
                delta.x,
                self.view_image_size.x,
                self.view_size.x,
            ),
            axis_drag(
                current.y,
                delta.y,
                self.view_image_size.y,
                self.view_size.y,
            ),
        );
        // 拖拽偏移并入新的锚点。
        let anchor = self.anchor_pan();
        self.pan_drag = next - anchor;
    }

    /// 底部控制栏：图标按钮与当前图片信息。
    fn controls(&mut self, ui: &mut egui::Ui) {
        let fg = self.settings.background.foreground();
        let mut toggle_settings = false;
        ui.horizontal(|ui| {
            let response = icons::button(ui, Icon::Folder, icons::ICON_SIZE, fg, true)
                .on_hover_text("打开图片");
            if response.clicked() {
                self.open_dialog();
            }
            ui.separator();
            let enabled = !self.gallery.is_empty();
            // 列表不循环：首图没有上一张，末图没有下一张。
            let has_prev = self.index > 0;
            let has_next = self.index + 1 < self.gallery.len();
            let response = icons::button(ui, Icon::ChevronLeft, icons::ICON_SIZE, fg, has_prev)
                .on_hover_text("上一张");
            if response.clicked() {
                self.step(-1);
            }
            let response = icons::button(ui, Icon::ChevronRight, icons::ICON_SIZE, fg, has_next)
                .on_hover_text("下一张");
            if response.clicked() {
                self.step(1);
            }
            ui.separator();
            let response = icons::button(ui, Icon::ZoomOut, icons::ICON_SIZE, fg, enabled)
                .on_hover_text("缩小");
            if response.clicked() {
                self.zoom_step(-1, None);
            }
            let response = icons::button(ui, Icon::ZoomIn, icons::ICON_SIZE, fg, enabled)
                .on_hover_text("放大");
            if response.clicked() {
                self.zoom_step(1, None);
            }
            let response = icons::button(ui, Icon::Fit, icons::ICON_SIZE, fg, enabled)
                .on_hover_text("适应窗口");
            if response.clicked() {
                self.reset_view();
            }
            ui.separator();
            let response = icons::button(ui, Icon::TextScan, icons::ICON_SIZE, fg, enabled)
                .on_hover_text("识别图片中的文字");
            if response.clicked() {
                self.run_ocr();
            }
            let response = icons::button(ui, Icon::Edit, icons::ICON_SIZE, fg, self.source.is_some())
                .on_hover_text("编辑图片");
            if response.clicked() {
                self.edit_open = !self.edit_open;
                // 三个浮动窗口都固定在右下角，同时打开会互相遮挡。
                if self.edit_open {
                    self.settings_open = false;
                    self.anno_open = false;
                    self.anno_shape = None;
                }
            }
            let response = icons::button(ui, Icon::Annotate, icons::ICON_SIZE, fg, self.source.is_some())
                .on_hover_text("注释图片");
            if response.clicked() {
                self.anno_open = !self.anno_open;
                if self.anno_open {
                    self.settings_open = false;
                    self.edit_open = false;
                } else {
                    // 收起注释窗口即结束注释模式。
                    self.anno_shape = None;
                }
            }
            ui.separator();
            ui.label(format!("{:.0}%", self.zoom_target));
            ui.label(self.status_text());
            // 识别进行时在状态栏显示进度条。
            if self.ocr_running {
                ui.add_space(6.0);
                ui.add(
                    egui::ProgressBar::new(self.ocr_progress.clamp(0.0, 1.0))
                        .desired_width(120.0)
                        .text(self.ocr_stage.clone()),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let response = icons::button(ui, Icon::Settings, icons::ICON_SIZE, fg, true)
                    .on_hover_text("设置");
                toggle_settings = response.clicked();
            });
        });
        if toggle_settings {
            self.settings_open = !self.settings_open;
            // 三个浮动窗口都固定在右下角，同时打开会互相遮挡。
            if self.settings_open {
                self.edit_open = false;
                self.anno_open = false;
                self.anno_shape = None;
            }
        }
    }

    fn status_text(&self) -> String {
        if let Some(err) = &self.status {
            return err.clone();
        }
        // 启动时列表还没建好：跟着画布一起提示加载状态。
        if self.gallery.is_empty() {
            return if self.loading {
                "正在加载…".to_string()
            } else {
                "未打开图片".to_string()
            };
        }
        format!(
            "{}/{}  {}",
            self.index + 1,
            self.gallery.len(),
            self.current_name()
        )
    }

    fn open_dialog(&mut self) {
        // 系统文件对话框是阻塞调用，单独计时。
        perf_scope!("打开文件对话框");
        let picked = rfd::FileDialog::new()
            .add_filter(
                "图片",
                &[
                    "jpg", "jpeg", "png", "gif", "bmp", "webp", "tiff", "tif", "ico", "qoi", "tga",
                    "dds", "hdr", "exr",
                ],
            )
            .pick_file();
        if let Some(path) = picked {
            self.open(&path);
        }
    }

    /// 设置面板：背景色、缓存张数、控制栏显隐、缩放步进与图片内边距。
    fn settings_window(&mut self, ctx: &egui::Context) -> Option<egui::Rect> {
        if !self.settings_open {
            return None;
        }
        let bg = self.settings.background;
        let fg = bg.foreground();
        let mut open = self.settings_open;
        let mut close_clicked = false;
        let old = self.settings;
        let shown = egui::Window::new("设置")
            .open(&mut open)
            .resizable(false)
            .movable(false)
            .title_bar(false)
            .frame(window_frame(ctx, bg))
            // 固定在右下角，并避开底部控制栏。
            .anchor(
                egui::Align2::RIGHT_BOTTOM,
                egui::vec2(-WINDOW_MARGIN, -(WINDOW_MARGIN + self.controls_height)),
            )
            .show(ctx, |ui| {
                close_clicked = compact_title(ui, "设置", fg);
                egui::Grid::new("settings_grid")
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label("背景色");
                        egui::ComboBox::from_id_salt("background")
                            .selected_text(self.settings.background.label())
                            .show_ui(ui, |ui| {
                                for option in Background::ALL {
                                    let selected = option == self.settings.background;
                                    if ui.selectable_label(selected, option.label()).clicked() {
                                        self.settings.background = option;
                                    }
                                }
                            });
                        ui.end_row();

                        ui.label("缓存张数");
                        stepper(ui, &mut self.settings.cache_count, 0..=20, 1.0, "", fg);
                        ui.end_row();

                        ui.label("隐藏控制栏");
                        ui.checkbox(&mut self.settings.hide_controls, "鼠标移到底部时显示");
                        ui.end_row();

                        ui.label("缩放步进");
                        stepper(ui, &mut self.settings.zoom_step, 1.0..=100.0, 1.0, "%", fg);
                        ui.end_row();

                        ui.label("图片内边距");
                        stepper(ui, &mut self.settings.padding, 0..=200, 1.0, "px", fg);
                        ui.end_row();

                        ui.label("OCR 框颜色");
                        egui::ComboBox::from_id_salt("ocr_box_color")
                            .selected_text(self.settings.ocr_box_color.label())
                            .show_ui(ui, |ui| {
                                for option in BoxColor::ALL {
                                    let selected = option == self.settings.ocr_box_color;
                                    if ui.selectable_label(selected, option.label()).clicked() {
                                        self.settings.ocr_box_color = option;
                                    }
                                }
                            });
                        ui.end_row();

                        ui.label("OCR 缓存张数");
                        stepper(ui, &mut self.settings.ocr_cache_cap, 1..=500, 1.0, "", fg);
                        ui.end_row();

                        ui.label("自动保存编辑");
                        ui.checkbox(&mut self.settings.auto_save_edit, "切换图片时保存未保存的编辑");
                        ui.end_row();
                    });
            });
        if close_clicked {
            open = false;
        }
        self.settings_open = open;
        if self.settings != old {
            self.settings_dirty = true;
            eprintln!("[settings] 变更: {old:?} -> {:?}", self.settings);
        }
        if self.settings.cache_count != old.cache_count {
            eprintln!("[settings] cache_count -> {}", self.settings.cache_count);
            self.apply_cache_settings();
            self.prefetch_neighbors();
        }
        if self.settings.ocr_cache_cap != old.ocr_cache_cap {
            eprintln!("[settings] ocr_cache_cap -> {}", self.settings.ocr_cache_cap);
            self.ocr_cache.set_cap(self.settings.ocr_cache_cap);
        }
        shown.map(|inner| inner.response.rect)
    }

    /// 编辑面板：缩放、翻转、旋转、黑白、亮度对比度，底部为保存按钮。
    fn edit_window(&mut self, ctx: &egui::Context) {
        if !self.edit_open {
            return;
        }
        let bg = self.settings.background;
        let fg = bg.foreground();
        let mut open = self.edit_open;
        let mut edit = self.edit;
        let mut auto_save = self.settings.auto_save_edit;
        let mut save = false;
        let mut reset = false;
        let mut close_clicked = false;
        let has_image = self.source.is_some();

        egui::Window::new("编辑图片")
            .open(&mut open)
            .resizable(false)
            .movable(false)
            .title_bar(false)
            .frame(window_frame(ctx, bg))
            // 固定在右下角，并避开底部控制栏。
            .anchor(
                egui::Align2::RIGHT_BOTTOM,
                egui::vec2(-WINDOW_MARGIN, -(WINDOW_MARGIN + self.controls_height)),
            )
            .show(ctx, |ui| {
                close_clicked = compact_title(ui, "编辑图片", fg);
                ui.add_enabled_ui(has_image, |ui| {
                ui.label("缩放");
                ui.horizontal(|ui| {
                    if ui.selectable_label(edit.scale.is_none(), "原始").clicked() {
                        edit.scale = None;
                    }
                    for pct in editor::SCALE_STEPS {
                        if ui.selectable_label(edit.scale == Some(pct), format!("{pct}%")).clicked() {
                            edit.scale = Some(pct);
                        }
                    }
                });
                ui.horizontal(|ui| {
                    if ui.selectable_label(edit.flip_h, "水平翻转").clicked() {
                        edit.flip_h = !edit.flip_h;
                    }
                    if ui.selectable_label(edit.flip_v, "垂直翻转").clicked() {
                        edit.flip_v = !edit.flip_v;
                    }
                });
                ui.horizontal(|ui| {
                    for deg in [90u16, 180, 270] {
                        if ui.button(format!("{deg}°")).clicked() {
                            edit.rotate = (edit.rotate + deg) % 360;
                        }
                    }
                    ui.label(format!("当前 {}°", edit.rotate));
                });
                if ui.selectable_label(edit.grayscale, "黑白").clicked() {
                    edit.grayscale = !edit.grayscale;
                }
                ui.label("亮度");
                ui.add(egui::Slider::new(&mut edit.brightness, -100.0..=100.0).suffix("%"));
                ui.label("对比度");
                ui.add(egui::Slider::new(&mut edit.contrast, -100.0..=100.0).suffix("%"));
                ui.checkbox(&mut auto_save, "切换图片时自动保存");
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("重置").clicked() {
                        reset = true;
                    }
                    if ui.button("保存到原图").clicked() {
                        save = true;
                    }
                });
                });
            });

        if close_clicked {
            open = false;
        }
        self.edit_open = open;
        if auto_save != self.settings.auto_save_edit {
            self.settings.auto_save_edit = auto_save;
            self.settings_dirty = true;
            eprintln!("[edit] 自动保存 -> {auto_save}");
        }
        if reset {
            edit = EditState::default();
        }
        if edit != self.edit {
            self.edit = edit;
            // 修改立即生效：刷新预览并作废旧的识别结果。
            self.refresh_preview();
            self.clear_ocr();
        }
        if save {
            self.save_edits();
        }
    }

    /// 注释面板：形状、颜色、粗细与保存操作，样式与编辑图片窗口一致。
    fn annotation_window(&mut self, ctx: &egui::Context) {
        if !self.anno_open {
            return;
        }
        let bg = self.settings.background;
        let fg = bg.foreground();
        let mut open = self.anno_open;
        let mut shape = self.anno_shape;
        let mut color = self.anno_color;
        let mut width = self.anno_width;
        let mut auto_save = self.settings.auto_save_annotation;
        let mut clear = false;
        let mut save = false;
        let mut close_clicked = false;

        egui::Window::new("注释")
            .open(&mut open)
            .resizable(false)
            .movable(false)
            .title_bar(false)
            .frame(window_frame(ctx, bg))
            // 固定在右下角，并避开底部控制栏。
            .anchor(
                egui::Align2::RIGHT_BOTTOM,
                egui::vec2(-WINDOW_MARGIN, -(WINDOW_MARGIN + self.controls_height)),
            )
            .show(ctx, |ui| {
                close_clicked = compact_title(ui, "注释", fg);
                let has_image = self.source.is_some();
                ui.add_enabled_ui(has_image, |ui| {
                    // 形状：点一下即进入该形状的注释模式。
                    ui.horizontal(|ui| {
                        for option in annotation::Shape::ALL {
                            if ui.selectable_label(shape == Some(option), option.label()).clicked() {
                                shape = Some(option);
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        for option in annotation::Color::ALL {
                            if color_swatch(ui, option, color == option, fg) {
                                color = option;
                            }
                        }
                    });
                    ui.label("线条粗细");
                    ui.add(egui::Slider::new(&mut width, 0.1..=10.0).suffix("%"));
                    ui.checkbox(&mut auto_save, "切换图片时自动保存");
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("清空注释").clicked() {
                            clear = true;
                        }
                        if ui.button("保存到原图").clicked() {
                            save = true;
                        }
                    });
                });
            });

        if close_clicked {
            open = false;
        }
        self.anno_open = open;
        self.anno_shape = shape;
        self.anno_color = color;
        self.anno_width = width;
        // 窗口收起即结束注释模式，避免残留的形状等待下一次落笔。
        if !self.anno_open {
            self.anno_shape = None;
        }
        if auto_save != self.settings.auto_save_annotation {
            self.settings.auto_save_annotation = auto_save;
            self.settings_dirty = true;
            eprintln!("[anno] 自动保存 -> {auto_save}");
        }
        if clear {
            self.clear_annotations();
            self.status = Some("已清空注释".to_string());
        }
        if save {
            self.save_annotations();
        }
    }

    /// 换图确认框：提示注释会被清空，由用户决定是否继续换图。
    fn switch_confirm(&mut self, ctx: &egui::Context) {
        if self.pending_switch.is_none() {
            return;
        }
        let bg = self.settings.background;
        let mut cancel = false;
        let mut confirm = false;
        let response = egui::Modal::new(egui::Id::unique("anno_switch_confirm"))
            .frame(window_frame(ctx, bg))
            .show(ctx, |ui| {
                ui.label("切换图片会清空当前图片的注释");
                ui.horizontal(|ui| {
                    if ui.button("取消").clicked() {
                        cancel = true;
                    }
                    if ui.button("确定").clicked() {
                        confirm = true;
                    }
                });
            });
        if cancel {
            self.pending_switch = None;
            eprintln!("[anno] 取消换图，保留当前图片的注释");
            return;
        }
        if confirm {
            let intent = self.pending_switch.take();
            // 注释先清空，再次换图不会重复弹框。
            self.clear_annotations();
            match intent {
                Some(SwitchIntent::Step(delta)) => {
                    self.step(delta);
                }
                Some(SwitchIntent::Open(path)) => self.open(&path),
                None => {}
            }
            return;
        }
        // 点背景或按 Esc 关闭：等同取消。
        if response.should_close() {
            self.pending_switch = None;
            eprintln!("[anno] 关闭确认框，取消换图");
        }
    }

    /// 右上角文本弹窗：显示选中文本框的内容，并提供复制按钮。
    fn ocr_popup(&mut self, ctx: &egui::Context) {
        if !self.ocr_popup_open {
            return;
        }
        let Some(index) = self.ocr_selected else {
            self.ocr_popup_open = false;
            return;
        };
        let Some(item) = self.ocr_boxes.get(index) else {
            self.ocr_popup_open = false;
            return;
        };
        let text = item.text.clone();
        let score = item.score;
        let bg = self.settings.background;
        let fg = bg.foreground();
        let mut open = true;
        let mut copy = false;
        let mut close_clicked = false;
        // 弹窗内可编辑副本，允许手动选中复制。
        let mut editable = text.clone();
        egui::Window::new("识别文字")
            .open(&mut open)
            .resizable(false)
            .movable(false)
            .title_bar(false)
            .frame(window_frame(ctx, bg))
            .anchor(
                egui::Align2::RIGHT_TOP,
                egui::vec2(-WINDOW_MARGIN, WINDOW_MARGIN),
            )
            .show(ctx, |ui| {
                close_clicked = compact_title(ui, "识别文字", fg);
                ui.label(format!("置信度 {:.1}%", score * 100.0));
                ui.separator();
                if text.is_empty() {
                    ui.label("（未识别到文字）");
                } else {
                    ui.add(
                        egui::TextEdit::multiline(&mut editable)
                            .desired_width(280.0)
                            .font(egui::TextStyle::Monospace),
                    );
                }
                ui.separator();
                if ui.button("复制").clicked() {
                    copy = true;
                }
            });
        if copy {
            ctx.copy_text(text);
            self.status = Some("已复制到剪贴板".to_string());
        }
        if close_clicked {
            open = false;
        }
        self.ocr_popup_open = open;
    }

    /// 方向键与 PageUp/PageDown 切换图片。
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        // 换图确认框弹出时忽略方向键，避免连按连切。
        if self.pending_switch.is_some() {
            return;
        }
        if self.settings_open {
            return;
        }
        // 识别进行中忽略换图快捷键。
        if self.ocr_running {
            return;
        }
        let previous = ctx.input(|i| {
            i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::PageUp)
        });
        let next = ctx.input(|i| {
            i.key_pressed(egui::Key::ArrowRight) || i.key_pressed(egui::Key::PageDown)
        });
        if previous {
            self.step(-1);
        }
        if next {
            self.step(1);
        }
    }

    /// 支持把图片拖进窗口。
    fn handle_drop(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx
            .input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if let Some(path) = dropped.into_iter().find(|path| path.is_file()) {
            self.open(&path);
        }
    }

    /// 控制栏是否显示：开启隐藏后，鼠标靠近窗口底边才唤出。
    fn controls_visible(&self, ctx: &egui::Context) -> bool {
        if !self.settings.hide_controls {
            return true;
        }
        let ppp = ctx.pixels_per_point();
        let viewport = ctx.viewport_rect();
        let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else {
            return false;
        };
        let in_x = pos.x >= viewport.min.x && pos.x <= viewport.max.x;
        in_x && pos.y >= viewport.max.y - BOTTOM_REVEAL_PX / ppp
    }
}

impl eframe::App for PhotoViewer {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // 帧计时只在 debug 版本统计。
        #[cfg(debug_assertions)]
        let frame_started = Instant::now();
        let ctx = ui.ctx().clone();
        // 待打开的图片先在后台扫描目录，结果回来后再提交解码。
        self.pump_open();
        self.poll(&ctx);
        self.poll_ocr(&ctx);
        self.poll_editor();
        self.handle_shortcuts(&ctx);
        self.handle_drop(&ctx);

        let background = self.settings.background.color();
        // 背景色变了才重设配色：全局样式管窗口与弹出菜单，
        // 根 Ui 的样式管控制栏，两者一起换才不会有一帧的旧配色。
        if self.styled_background != Some(self.settings.background) {
            let bg = self.settings.background;
            ctx.global_style_mut(|style| apply_background_style(style, bg));
            apply_background_style(ui.style_mut(), bg);
            self.styled_background = Some(bg);
            eprintln!("[style] 配色跟随背景色: {}", bg.label());
        }
        // 先画控制栏：浮动窗口要按它的高度上移，避免互相遮挡。
        if self.controls_visible(&ctx) {
            let response = egui::Panel::bottom("controls")
                .frame(
                    egui::Frame::new()
                        .fill(background)
                        .inner_margin(egui::Margin::symmetric(8, 6)),
                )
                .show(ui, |ui| self.controls(ui));
            self.controls_height = response.response.rect.height();
        }
        let _ = self.settings_window(&ctx);
        self.edit_window(&ctx);
        self.annotation_window(&ctx);
        self.ocr_popup(&ctx);
        // 换图确认框最后画，保证盖在其它浮动窗口之上。
        self.switch_confirm(&ctx);

        // 设置改动后立刻落盘，避免异常退出丢失。
        if self.settings_dirty {
            if let Some(storage) = frame.storage_mut() {
                self.persist_settings(storage);
            }
            self.settings_dirty = false;
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(background))
            .show(ui, |ui| self.canvas(ui));

        // 帧耗时：首帧与慢帧单独记录，用来定位界面卡顿。
        self.frames += 1;
        #[cfg(debug_assertions)]
        {
            let frame_ms = crate::perf::ms(frame_started.elapsed());
            if self.frames == 1 {
                perf_log!("首帧渲染完成，耗时 {:.2}ms", frame_ms);
            } else if frame_ms > SLOW_FRAME_MS {
                perf_log!("慢帧 #{} 耗时 {:.2}ms", self.frames, frame_ms);
            }
        }
    }

    /// 退出时保存设置（窗口位置/大小/最大化状态由 eframe 自行保存）。
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eprintln!("[settings] 退出前保存设置");
        self.settings.save(storage);
    }
}

/// 判断按下位置落在画布的哪个边缘。
fn edge_of(view: egui::Rect, pos: egui::Pos2, ppp: f32) -> Edge {
    let edge = EDGE_PX / ppp;
    if view.width() <= edge * 2.0 {
        return Edge::None;
    }
    if pos.x <= view.left() + edge {
        Edge::Left
    } else if pos.x >= view.right() - edge {
        Edge::Right
    } else {
        Edge::None
    }
}

/// 单轴缩放锚点：图片超出视口时以光标为中心缩放，越接近视口大小时越偏向居中。
fn axis_anchor(cursor: f32, offset: f32, image: f32, view: f32) -> f32 {
    axis_blend(image, view) * (cursor - offset)
}

/// 单轴的居中混合系数：图片与视口等大时为 0（完全居中），超出足够多时为 1（完全跟随光标）。
///
/// 系数在过渡区间内平滑变化，缩到与视口等大时偏移连续地回到 0，不会突然跳变。
fn axis_blend(image: f32, view: f32) -> f32 {
    if view <= 0.0 {
        return 1.0;
    }
    let over = image - view;
    if over <= 0.0 {
        return 0.0;
    }
    let u = (over / (view * BLEND_RATIO)).clamp(0.0, 1.0);
    // 平滑插值：过渡两端速度为 0，接入与脱离光标锚点时都没有突变。
    u * u * (3.0 - 2.0 * u)
}

/// 单轴拖拽结果：图片未超出视口时回到视口中心，超出时限制在齐平范围内。
fn axis_drag(center: f32, delta: f32, image: f32, view: f32) -> f32 {
    let half = image * 0.5;
    let half_view = view * 0.5;
    if image <= view {
        return 0.0;
    }
    let (min, max) = drag_range(center, half, half_view);
    (center + delta).clamp(min, max)
}

/// 单轴的可拖动范围：图片露出视口的一侧决定方向，两侧都露出时按盖住视口处理。
fn drag_range(center: f32, half: f32, half_view: f32) -> (f32, f32) {
    let top_out = center - half < -half_view;
    let bottom_out = center + half > half_view;
    let min = if bottom_out { half_view - half } else { center };
    let max = if top_out { half - half_view } else { center };
    (min, max)
}

/// 射线法判断点是否落在多边形内。
fn point_in_polygon(pos: egui::Pos2, points: &[egui::Pos2]) -> bool {
    if points.len() < 3 {
        return false;
    }
    let mut inside = false;
    for i in 0..points.len() {
        let a = points[i];
        let b = points[(i + 1) % points.len()];
        // 判断边是否跨越水平射线。
        if (a.y > pos.y) != (b.y > pos.y) {
            let x = (b.x - a.x) * (pos.y - a.y) / (b.y - a.y) + a.x;
            if pos.x < x {
                inside = !inside;
            }
        }
    }
    inside
}

/// 颜色圆圈：实心圆表示颜色，选中时外加一圈描边。
fn color_swatch(
    ui: &mut egui::Ui,
    color: annotation::Color,
    selected: bool,
    fg: egui::Color32,
) -> bool {
    const SIZE: f32 = 18.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(SIZE, SIZE), egui::Sense::click());
    let center = rect.center();
    let radius = SIZE * 0.5 - 2.0;
    ui.painter().circle_filled(center, radius, color.color());
    if selected {
        ui.painter()
            .circle_stroke(center, radius + 1.5, egui::Stroke::new(1.5, fg));
    }
    // 悬浮提示给出颜色名，方便区分相近颜色。
    response.on_hover_text(color.label()).clicked()
}

/// 屏幕坐标转图片内的归一化坐标（0~1，原点在左上角）。
fn normalized_point(pos: egui::Pos2, rect: egui::Rect) -> egui::Vec2 {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return egui::Vec2::ZERO;
    }
    egui::vec2(
        ((pos.x - rect.min.x) / rect.width()).clamp(0.0, 1.0),
        ((pos.y - rect.min.y) / rect.height()).clamp(0.0, 1.0),
    )
}

/// 把屏幕坐标夹回图片区域内。
fn clamp_into_rect(pos: egui::Pos2, rect: egui::Rect) -> egui::Pos2 {
    egui::pos2(
        pos.x.clamp(rect.min.x, rect.max.x),
        pos.y.clamp(rect.min.y, rect.max.y),
    )
}

/// 浮动窗口的外框：底色跟随设置里的背景色。
fn window_frame(ctx: &egui::Context, bg: Background) -> egui::Frame {
    let fg = bg.foreground();
    egui::Frame::window(&ctx.global_style())
        .fill(bg.color())
        .stroke(egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(fg.r(), fg.g(), fg.b(), 60),
        ))
        .inner_margin(egui::Margin::symmetric(8, 6))
}

/// 让 egui 的配色跟随背景色：窗口、弹出菜单与文字统一用同一套颜色。
fn apply_background_style(style: &mut egui::Style, bg: Background) {
    let fg = bg.foreground();
    let fill = bg.widget_fill();
    let hover = bg.widget_fill_hover();
    let visuals = &mut style.visuals;
    visuals.override_text_color = Some(fg);
    visuals.faint_bg_color = fill;
    visuals.extreme_bg_color = fill;
    visuals.text_edit_bg_color = Some(fill);
    visuals.window_fill = bg.color();
    visuals.panel_fill = bg.color();
    for (widget, color) in [
        (&mut visuals.widgets.noninteractive, fill),
        (&mut visuals.widgets.inactive, fill),
        (&mut visuals.widgets.hovered, hover),
        (&mut visuals.widgets.active, hover),
        (&mut visuals.widgets.open, hover),
    ] {
        widget.bg_fill = color;
        widget.weak_bg_fill = color;
        widget.fg_stroke = egui::Stroke::new(1.0, fg);
    }
}

/// 自绘的紧凑标题栏：标题在左、关闭按钮在右，比 egui 默认标题栏略矮。
fn compact_title(ui: &mut egui::Ui, title: &str, fg: egui::Color32) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        ui.label(title);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if icons::button(ui, Icon::Close, CLOSE_ICON_SIZE, fg, true).clicked() {
                close = true;
            }
        });
    });
    close
}

/// 数值调节控件：左右箭头按钮加可拖拽的数值框，按钮按步长增减并限制在范围内。
fn stepper<T: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    value: &mut T,
    range: std::ops::RangeInclusive<T>,
    step: f64,
    suffix: &str,
    fg: egui::Color32,
) {
    let (min, max) = (range.start().to_f64(), range.end().to_f64());
    ui.horizontal(|ui| {
        if icons::button(ui, Icon::ChevronLeft, STEP_ICON_SIZE, fg, true).clicked() {
            *value = T::from_f64((value.to_f64() - step).clamp(min, max));
        }
        let mut drag = egui::DragValue::new(value).range(range);
        if !suffix.is_empty() {
            drag = drag.suffix(suffix);
        }
        ui.add(drag);
        if icons::button(ui, Icon::ChevronRight, STEP_ICON_SIZE, fg, true).clicked() {
            *value = T::from_f64((value.to_f64() + step).clamp(min, max));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotation_is_drawn_at_the_click_position() {
        // 图片不在屏幕原点时，点击位置与绘制位置必须一致。
        let rect = egui::Rect::from_min_max(egui::pos2(580.0, 25.0), egui::pos2(1325.0, 930.0));
        let click = egui::pos2(900.0, 400.0);
        let norm = normalized_point(click, rect);
        let drawn = annotation::screen_point(norm, rect);
        assert!(
            (drawn - click).length() < 1e-3,
            "点击位置 {:?} 应与绘制位置 {:?} 一致",
            click,
            drawn
        );
        let geo = annotation::screen_geometry(
            annotation::Shape::Rect,
            norm,
            norm,
            rect,
            2.0,
        );
        assert!(
            (geo.strokes[0][0] - click.to_vec2()).length() < 1e-3,
            "矩形首点应落在点击位置: {:?}",
            geo.strokes[0][0]
        );
    }

    /// 等待后台目录扫描完成。
    fn wait_for_open(app: &mut PhotoViewer) {
        app.pump_open();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.open_rx.is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
            app.pump_open();
        }
        assert!(app.open_rx.is_none(), "目录扫描应在超时前完成");
    }

    /// 打开图片并等待目录扫描完成。
    fn open_and_wait(app: &mut PhotoViewer, path: &Path) {
        app.open(path);
        wait_for_open(app);
    }

    /// 图片列表不循环：首尾停住，不绕回另一端。
    #[test]
    fn step_stops_at_list_ends() {
        let dir = std::env::temp_dir().join("photo-viewer-viewer-test");
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png", "c.png"] {
            image::RgbaImage::new(4, 4).save(dir.join(name)).unwrap();
        }

        let mut app = PhotoViewer::new();
        open_and_wait(&mut app, &dir.join("b.png"));
        assert_eq!(app.gallery.len(), 3);
        assert_eq!(app.current_name(), "b.png");

        app.step(1);
        assert_eq!(app.current_name(), "c.png");
        // 末图没有下一张：停在原地。
        app.step(1);
        assert_eq!(app.current_name(), "c.png");
        app.step(-1);
        assert_eq!(app.current_name(), "b.png");
        app.step(-1);
        assert_eq!(app.current_name(), "a.png");
        // 首图没有上一张：停在原地。
        app.step(-1);
        assert_eq!(app.current_name(), "a.png");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn zoom_step_uses_anchor_ladder_with_hundred_percent_gear() {
        let mut app = PhotoViewer::new();
        app.zoom = 85.0;
        app.zoom_target = 85.0;
        app.zoom_base = 85.0;
        app.zoom_manual = true;

        app.zoom_step(1, None);
        assert_eq!(app.zoom_target, 95.0);
        app.zoom_step(1, None);
        assert_eq!(app.zoom_target, 100.0);
        app.zoom_step(1, None);
        assert_eq!(app.zoom_target, 110.0);
        app.zoom_step(1, None);
        assert_eq!(app.zoom_target, 120.0);
        app.zoom_step(-1, None);
        assert_eq!(app.zoom_target, 110.0);
    }

    /// 缩放后光标下的图像点保持不动。
    #[test]
    fn zoom_keeps_point_under_cursor() {
        let mut app = PhotoViewer::new();
        app.zoom = 100.0;
        app.zoom_target = 100.0;
        app.zoom_manual = true;
        app.view_center = egui::vec2(500.0, 300.0);
        app.view_size = egui::vec2(400.0, 300.0);
        app.view_image_size = egui::vec2(600.0, 400.0);

        let mouse = egui::pos2(700.0, 300.0);
        let before = (mouse - app.view_center) * (100.0 / app.zoom);
        app.zoom_step(1, Some(mouse));
        assert_eq!(app.zoom_target, 110.0);

        // 动画收敛到目标比例后，锚点处的图像点应保持不变。
        app.zoom = app.zoom_target;
        let after = (mouse - app.view_center - app.anchor_pan()) * (100.0 / app.zoom);
        assert!((after.x - before.x).abs() < 0.01 && (after.y - before.y).abs() < 0.01);
    }

    /// 光标在图片之外时，锚点被夹到图片范围内。
    #[test]
    fn zoom_anchor_is_clamped_into_image() {
        let mut app = PhotoViewer::new();
        app.zoom = 100.0;
        app.zoom_target = 100.0;
        app.zoom_manual = true;
        app.view_center = egui::vec2(500.0, 300.0);
        app.view_size = egui::vec2(400.0, 300.0);
        app.view_image_size = egui::vec2(600.0, 400.0);

        // 光标远在图片右侧之外。
        app.zoom_step(1, Some(egui::pos2(900.0, 300.0)));
        let (cursor, _) = app.zoom_anchor.unwrap();
        assert!(cursor.x <= 300.0 + 0.01 && cursor.x >= 300.0 - 0.01);
    }

    /// 图片未超出视口的轴固定居中，超出的轴才跟随光标。
    #[test]
    fn zoom_centers_axis_that_fits_viewport() {
        let mut app = PhotoViewer::new();
        app.zoom = 100.0;
        app.zoom_target = 100.0;
        app.zoom_manual = true;
        app.view_center = egui::vec2(300.0, 300.0);
        app.view_size = egui::vec2(600.0, 600.0);
        // 图片比视口窄但比视口高：横向应居中，纵向跟随光标。
        app.view_image_size = egui::vec2(400.0, 900.0);

        app.zoom_step(1, Some(egui::pos2(450.0, 500.0)));
        // 动画收敛到目标比例后再看偏移。
        app.zoom = app.zoom_target;
        let pan = app.anchor_pan();
        assert_eq!(pan.x, 0.0, "横向未超出视口时应居中");
        assert!(pan.y.abs() > 0.0, "纵向超出视口时应跟随光标");
    }

    /// 缩小到小于视口后，图片回到视口中心，不会超出视口边界。
    #[test]
    fn zoom_out_below_fit_centers_image() {
        let mut app = PhotoViewer::new();
        app.zoom = 100.0;
        app.zoom_target = 100.0;
        app.zoom_manual = true;
        app.view_center = egui::vec2(300.0, 300.0);
        app.view_size = egui::vec2(600.0, 600.0);
        // 图片此时比视口大，按光标缩放。
        app.view_image_size = egui::vec2(900.0, 900.0);
        app.zoom_step(1, Some(egui::pos2(500.0, 400.0)));

        // 缩小到图片小于视口：两个轴都应回到中心。
        app.view_image_size = egui::vec2(400.0, 400.0);
        let pan = app.image_pan();
        assert_eq!(pan.x, 0.0);
        assert_eq!(pan.y, 0.0);

        // 图片整体落在视口内。
        let half = app.view_image_size * 0.5;
        let half_view = app.view_size * 0.5;
        assert!(half.x <= half_view.x && half.y <= half_view.y);
    }

    /// 居中系数在图片接近视口大小时平滑变化，偏移连续地回到 0。
    #[test]
    fn anchor_blends_to_center_continuously() {
        let view = 600.0;
        let cursor = 300.0;
        let offset = 120.0;

        // 图片与视口等大时完全居中。
        assert_eq!(axis_anchor(cursor, offset, view, view), 0.0);

        // 图片刚超出视口时，偏移应小于完全跟随光标时的值。
        let near = axis_anchor(cursor, offset, view + 30.0, view);
        let far = axis_anchor(cursor, offset, view + 600.0, view);
        assert!(near > 0.0 && near < far, "过渡区应介于居中与跟随光标之间");

        // 过渡区内偏移单调变化，不出现跳变。
        let mut previous = axis_anchor(cursor, offset, view, view);
        for step in 1..=60 {
            let current = axis_anchor(cursor, offset, view + step as f32 * 2.0, view);
            assert!(
                current >= previous - 1e-4,
                "偏移应随图片变大而单调变化: {previous} -> {current}"
            );
            assert!(
                (current - previous).abs() < 20.0,
                "相邻采样点不应跳变: {previous} -> {current}"
            );
            previous = current;
        }
    }

    /// 只有超出视口的轴才能拖动。
    #[test]
    fn drag_moves_only_overflowing_axis() {
        let mut app = PhotoViewer::new();
        app.zoom = 100.0;
        app.view_size = egui::vec2(600.0, 600.0);

        // 图片比视口宽但不高：只能左右拖动。
        app.view_image_size = egui::vec2(1000.0, 400.0);
        app.apply_drag(egui::vec2(50.0, 30.0));
        assert_eq!(app.pan_drag.x, 50.0);
        assert_eq!(app.pan_drag.y, 0.0);
    }

    /// 图片完全落在视口内时，拖动会让图片回到视口中心。
    #[test]
    fn drag_recenters_image_inside_viewport() {
        let mut app = PhotoViewer::new();
        app.zoom = 100.0;
        app.view_size = egui::vec2(600.0, 600.0);
        // 图片宽 400（视口 600），整体偏左。
        app.view_image_size = egui::vec2(400.0, 400.0);
        app.pan_drag = egui::vec2(-150.0, 0.0);

        app.apply_drag(egui::vec2(-50.0, 0.0));
        assert_eq!(app.pan_drag.x, 0.0, "图片小于视口时应回到中心");
        assert_eq!(app.pan_drag.y, 0.0);
    }

    /// 拖动不能把图片拖出视口。
    #[test]
    fn drag_is_limited_to_overflow_range() {
        let mut app = PhotoViewer::new();
        app.zoom = 100.0;
        app.view_size = egui::vec2(600.0, 600.0);
        app.view_image_size = egui::vec2(1000.0, 400.0);

        app.apply_drag(egui::vec2(9999.0, 0.0));
        assert_eq!(app.pan_drag.x, 200.0);
    }

    /// 换图后的初始设定与刚打开该图时一致：缩放不沿用上一张的手动比例。
    #[test]
    fn switching_resets_zoom_to_initial_setting() {
        let dir = std::env::temp_dir().join("photo-viewer-zoom-reset-test");
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            image::RgbaImage::new(4, 4).save(dir.join(name)).unwrap();
        }

        // 参照：刚打开 b.png 时的初始状态。
        let mut fresh = PhotoViewer::new();
        open_and_wait(&mut fresh, &dir.join("b.png"));

        let mut app = PhotoViewer::new();
        open_and_wait(&mut app, &dir.join("a.png"));
        // 手动缩放到 100% 后换图。
        app.zoom_manual = true;
        app.zoom = 100.0;
        app.zoom_target = 100.0;
        app.zoom_base = 100.0;
        app.step(1);

        assert_eq!(app.current_name(), "b.png");
        assert!(!app.zoom_manual, "换图后不应保留手动缩放");
        assert_eq!(app.zoom_target, fresh.zoom_target);
        assert_eq!(app.zoom_base, fresh.zoom_base);
        assert_eq!(app.pan, fresh.pan);
        assert_eq!(app.pan_drag, fresh.pan_drag);
        assert!(app.zoom_anchor.is_none(), "换图后不应保留缩放锚点");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 缩放挡位基准取当前图片的适配比例，而非上一张图片的残留值。
    #[test]
    fn zoom_ladder_anchors_on_current_fit_ratio() {
        let mut app = PhotoViewer::new();
        app.fit_zoom = 40.0;
        app.zoom = 40.0;
        app.zoom_target = 40.0;

        app.zoom_step(1, None);
        assert_eq!(app.zoom_base, 40.0);
        assert_eq!(app.zoom_target, 50.0);
    }

    /// 在指定视口尺寸下跑一帧画布，走真实的渲染路径。
    fn run_canvas(app: &mut PhotoViewer, width: f32, height: f32, events: Vec<egui::Event>) {
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::empty());
        run_canvas_ctx(app, &ctx, width, height, events);
    }

    /// 复用同一个上下文跑一帧画布：跨帧的指针状态才能延续。
    fn run_canvas_ctx(
        app: &mut PhotoViewer,
        ctx: &egui::Context,
        width: f32,
        height: f32,
        events: Vec<egui::Event>,
    ) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, height),
            )),
            events,
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| app.canvas(ui));
        output.drop_without_applying_deltas();
    }

    /// 跑一帧画布，返回画布上画出的文字。
    fn canvas_texts(app: &mut PhotoViewer, width: f32, height: f32) -> Vec<String> {
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::empty());
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, height),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| app.canvas(ui));
        let texts = output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => Some(text.galley.job.text.clone()),
                _ => None,
            })
            .collect();
        output.drop_without_applying_deltas();
        texts
    }

    /// 滚轮放大一格的事件。
    fn wheel_event() -> egui::Event {
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 20.0),
            phase: egui::TouchPhase::End,
            source: egui::MouseWheelSource::Unknown,
            modifiers: egui::Modifiers::default(),
        }
    }

    /// 左键按下或抬起的事件。
    fn pointer_button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }
    }

    /// 真实画布路径：缩放到 100% 后换图，新图按自己的适配比例显示。
    #[test]
    fn canvas_restores_fit_zoom_after_switching() {
        let dir = std::env::temp_dir().join("photo-viewer-canvas-zoom-test");
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            image::RgbaImage::new(4, 4).save(dir.join(name)).unwrap();
        }

        let mut app = PhotoViewer::new();
        open_and_wait(&mut app, &dir.join("a.png"));
        // 模拟解码完成：a.png 是一张大图。
        app.source = Some(Arc::new(image::RgbaImage::new(2000, 1500)));
        app.content_size = [2000, 1500];

        // 首帧画布：按视口适配。
        run_canvas(&mut app, 800.0, 600.0, Vec::new());
        assert!((app.fit_zoom - 40.0).abs() < 0.01, "适配比例应为 40%");

        // 滚轮放大到 100%：每帧一格，走真实的输入路径。
        for _ in 0..6 {
            run_canvas(&mut app, 800.0, 600.0, vec![wheel_event()]);
        }
        assert!(app.zoom_manual, "滚轮缩放后应进入手动缩放");
        assert_eq!(app.zoom_target, 100.0);

        // 换图：初始设定回到适配视口，不沿用上一张的 100%。
        app.step(1);
        assert!(!app.zoom_manual, "换图后不应保留手动缩放");

        // 模拟 b.png 解码完成，再走一帧画布。
        app.source = Some(Arc::new(image::RgbaImage::new(1200, 1800)));
        app.content_size = [1200, 1800];
        run_canvas(&mut app, 800.0, 600.0, Vec::new());
        assert!(
            (app.zoom - 33.3333).abs() < 0.01,
            "新图应按自己的适配比例显示，实际 {}%",
            app.zoom
        );
        assert!((app.zoom - 100.0).abs() > 1.0, "新图不应沿用上一张的 100%");
        assert_eq!(app.zoom_target, app.zoom);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 边缘点击换图：本帧不能再按旧尺寸请求纹理，否则会多生成一张全分辨率纹理。
    #[test]
    fn edge_click_switch_skips_stale_texture_request() {
        let dir = std::env::temp_dir().join("photo-viewer-edge-click-test");
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            image::RgbaImage::new(4, 4).save(dir.join(name)).unwrap();
        }

        let mut app = PhotoViewer::new();
        open_and_wait(&mut app, &dir.join("a.png"));
        // 模拟解码完成：a.png 是一张大图。
        app.source = Some(Arc::new(image::RgbaImage::new(2000, 1500)));
        app.content_size = [2000, 1500];

        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::empty());
        // 先跑一帧让画布登记交互区：egui 的点击命中用的是上一帧的部件矩形。
        run_canvas_ctx(&mut app, &ctx, 800.0, 600.0, Vec::new());
        // 右边缘感应区内的按下与抬起。
        let pos = egui::pos2(797.0, 300.0);
        run_canvas_ctx(
            &mut app,
            &ctx,
            800.0,
            600.0,
            vec![pointer_button(pos, true)],
        );
        run_canvas_ctx(
            &mut app,
            &ctx,
            800.0,
            600.0,
            vec![pointer_button(pos, false)],
        );

        assert_eq!(app.current_name(), "b.png", "边缘点击应换到下一张");
        assert!(
            app.want_px.is_none(),
            "换图当帧不应按旧尺寸请求纹理，实际请求 {:?}",
            app.want_px
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 换图结果只报告真正换图的情况：边界与识别中都不算。
    #[test]
    fn step_reports_whether_image_changed() {
        let dir = std::env::temp_dir().join("photo-viewer-step-report-test");
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            image::RgbaImage::new(4, 4).save(dir.join(name)).unwrap();
        }

        let mut app = PhotoViewer::new();
        open_and_wait(&mut app, &dir.join("a.png"));
        assert!(app.step(1), "正常换图应报告成功");
        assert!(!app.step(1), "末图停留应报告未换图");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 真实画布路径：缩小到小于视口后，图片回到视口中心且不超出视口。
    #[test]
    fn canvas_centers_image_after_zoom_out_below_fit() {
        let dir = std::env::temp_dir().join("photo-viewer-canvas-fit-test");
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::new(4, 4).save(dir.join("a.png")).unwrap();

        let mut app = PhotoViewer::new();
        open_and_wait(&mut app, &dir.join("a.png"));
        app.source = Some(Arc::new(image::RgbaImage::new(2000, 1500)));
        app.content_size = [2000, 1500];

        // 首帧画布：按视口适配（800x600 → 40%），图片刚好占满视口。
        run_canvas(&mut app, 800.0, 600.0, Vec::new());
        assert!((app.fit_zoom - 40.0).abs() < 0.01, "适配比例应为 40%");

        // 缩小一格到 30%：图片 600x450 已小于视口，应居中显示。
        app.zoom_step(-1, None);
        app.zoom = app.zoom_target;
        run_canvas(&mut app, 800.0, 600.0, Vec::new());

        assert!(app.view_image_size.x <= app.view_size.x);
        assert!(app.view_image_size.y <= app.view_size.y);
        assert_eq!(app.pan.x, 0.0, "横向应与视口中心对齐");
        assert_eq!(app.pan.y, 0.0, "纵向应与视口中心对齐");

        // 图片矩形完全落在视口内。
        let rect = app.image_rect;
        let view = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        assert!(view.contains_rect(rect), "图片不应超出视口边界");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 内边距生效：适配视口时图片与窗口上下边缘留出设定间距。
    #[test]
    fn padding_shrinks_fit_area() {
        let dir = std::env::temp_dir().join("photo-viewer-padding-fit-test");
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::new(4, 4).save(dir.join("a.png")).unwrap();

        let mut app = PhotoViewer::new();
        app.settings.padding = 10;
        open_and_wait(&mut app, &dir.join("a.png"));
        app.source = Some(Arc::new(image::RgbaImage::new(2000, 1500)));
        app.content_size = [2000, 1500];

        run_canvas(&mut app, 800.0, 600.0, Vec::new());

        // 内容区为 780x580，图片按它适配，上下各留 10px。
        assert!(
            (app.image_rect.top() - 10.0).abs() < 0.01,
            "图片顶部应留出 10px，实际 {}",
            app.image_rect.top()
        );
        assert!(
            (600.0 - 10.0 - app.image_rect.bottom()).abs() < 0.01,
            "图片底部应留出 10px，实际 {}",
            app.image_rect.bottom()
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 内边距生效：图片拖到最边缘时与窗口仍保留设定间距。
    #[test]
    fn padding_keeps_gap_at_viewport_edge() {
        let dir = std::env::temp_dir().join("photo-viewer-padding-drag-test");
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::new(4, 4).save(dir.join("a.png")).unwrap();

        let mut app = PhotoViewer::new();
        app.settings.padding = 10;
        open_and_wait(&mut app, &dir.join("a.png"));
        app.source = Some(Arc::new(image::RgbaImage::new(3000, 2000)));
        app.content_size = [3000, 2000];
        // 手动放大到超出内容区，图片才能被拖动。
        app.zoom_manual = true;
        app.zoom = 60.0;
        app.zoom_target = 60.0;
        run_canvas(&mut app, 800.0, 600.0, Vec::new());

        // 拖到最右：图片左边缘留出 10px。
        app.apply_drag(egui::vec2(9999.0, 0.0));
        run_canvas(&mut app, 800.0, 600.0, Vec::new());
        assert!(
            (app.image_rect.left() - 10.0).abs() < 0.01,
            "图片左边缘应留出 10px，实际 {}",
            app.image_rect.left()
        );

        // 拖到最左：图片右边缘留出 10px。
        app.apply_drag(egui::vec2(-9999.0, 0.0));
        run_canvas(&mut app, 800.0, 600.0, Vec::new());
        assert!(
            (800.0 - 10.0 - app.image_rect.right()).abs() < 0.01,
            "图片右边缘应留出 10px，实际 {}",
            app.image_rect.right()
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 换图时设置与编辑窗口自动关闭。
    #[test]
    fn switching_closes_floating_windows() {
        let dir = std::env::temp_dir().join("photo-viewer-close-panels-test");
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            image::RgbaImage::new(4, 4).save(dir.join(name)).unwrap();
        }

        let mut app = PhotoViewer::new();
        open_and_wait(&mut app, &dir.join("a.png"));
        app.settings_open = true;
        app.edit_open = true;

        app.step(1);
        assert_eq!(app.current_name(), "b.png");
        assert!(!app.settings_open, "换图后设置窗口应关闭");
        assert!(!app.edit_open, "换图后编辑窗口应关闭");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 自绘的紧凑标题栏比 egui 默认标题栏更矮。
    #[test]
    fn compact_title_bar_is_shorter_than_default() {
        let ctx = egui::Context::default();
        let mut default_h = 0.0;
        let mut compact_h = 0.0;
        // 窗口首帧只测量尺寸，多跑几帧让它稳定。
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            };
            let output = ctx.run_pass(input, |ctx| {
                let default = egui::Window::new("默认").resizable(false).show(ctx, |ui| {
                    ui.label("内容");
                });
                let compact = egui::Window::new("紧凑")
                    .resizable(false)
                    .title_bar(false)
                    .show(ctx, |ui| {
                        compact_title(ui, "紧凑", egui::Color32::WHITE);
                        ui.label("内容");
                    });
                if let Some(window) = default {
                    default_h = window.response.rect.height();
                }
                if let Some(window) = compact {
                    compact_h = window.response.rect.height();
                }
            });
            output.drop_without_applying_deltas();
        }

        eprintln!("[test] 默认标题栏 {default_h}，紧凑标题栏 {compact_h}");
        assert!(
            compact_h < default_h,
            "紧凑标题栏应比默认矮：{compact_h} vs {default_h}"
        );
    }

    /// 控制栏高度被正确测量，浮动窗口据此避让。
    #[test]
    fn control_bar_height_is_measured() {
        let ctx = egui::Context::default();
        let mut app = PhotoViewer::new();
        let mut height = 0.0;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_pass(input, |ctx| {
            let response = ctx.root_ui(|ui| {
                egui::Panel::bottom("controls")
                    .frame(
                        egui::Frame::new()
                            .fill(egui::Color32::WHITE)
                            .inner_margin(egui::Margin::symmetric(8, 6)),
                    )
                    .show(ui, |ui| app.controls(ui))
            });
            height = response.response.rect.height();
        });
        output.drop_without_applying_deltas();

        // 图标按钮加上内边距，控制栏高度应在 20~80 点之间。
        assert!(
            (20.0..80.0).contains(&height),
            "控制栏高度测量异常：{height}"
        );
    }

    /// 浮动窗口固定在右下角，并避开底部控制栏。
    #[test]
    fn floating_window_avoids_control_bar() {
        let ctx = egui::Context::default();
        ctx.set_fonts(egui::FontDefinitions::empty());
        let mut app = PhotoViewer::new();
        app.settings_open = true;
        app.controls_height = 40.0;

        // 窗口首帧只测量尺寸，多跑几帧让它稳定。
        let mut rect = egui::Rect::ZERO;
        for _ in 0..4 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 800.0),
                )),
                ..Default::default()
            };
            let output = ctx.run_pass(input, |ctx| {
                rect = app.settings_window(ctx).unwrap_or(rect);
            });
            output.drop_without_applying_deltas();
        }

        assert!(
            (rect.right() - (1000.0 - WINDOW_MARGIN)).abs() < 1.0,
            "窗口应贴住右边缘，实际 {rect:?}"
        );
        assert!(
            (rect.bottom() - (800.0 - WINDOW_MARGIN - 40.0)).abs() < 1.0,
            "窗口底边应在控制栏之上，实际 {rect:?}"
        );
    }

    /// 真实画布路径：缩小到与视口等大时，偏移是渐进的，不会突然跳回中心。
    #[test]
    fn canvas_zoom_out_to_fit_moves_center_smoothly() {
        let dir = std::env::temp_dir().join("photo-viewer-canvas-smooth-test");
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::new(4, 4).save(dir.join("a.png")).unwrap();

        let mut app = PhotoViewer::new();
        open_and_wait(&mut app, &dir.join("a.png"));
        app.source = Some(Arc::new(image::RgbaImage::new(2000, 1500)));
        app.content_size = [2000, 1500];

        // 先按 100% 显示图片，锚点取靠右的鼠标位置。
        app.zoom_manual = true;
        app.zoom = 100.0;
        app.zoom_target = 100.0;
        run_canvas(&mut app, 800.0, 600.0, Vec::new());
        app.set_zoom_anchor(Some(egui::pos2(700.0, 300.0)));

        // 从 50% 逐步缩到 40%（图片刚好等于视口）。
        app.zoom = 50.0;
        app.zoom_target = 50.0;
        run_canvas(&mut app, 800.0, 600.0, Vec::new());
        let start = app.pan.x;

        let mut previous = start;
        let mut max_step = 0.0f32;
        let mut zoom = 50.0f32;
        while zoom > 40.0 {
            zoom -= 0.5;
            app.zoom = zoom;
            app.zoom_target = zoom;
            run_canvas(&mut app, 800.0, 600.0, Vec::new());
            let step = (app.pan.x - previous).abs();
            max_step = max_step.max(step);
            previous = app.pan.x;
        }

        let travel = (app.pan.x - start).abs();
        assert!(travel > 1.0, "本用例需要明显的偏移变化，实际 {travel}");
        assert!(
            max_step < travel * 0.3,
            "偏移跳变过大：最大步进 {max_step}，总位移 {travel}"
        );
        assert!(app.pan.x.abs() < 0.01, "缩到视口大小时应居中");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 识别进行中禁止切换图片。
    #[test]
    fn step_is_blocked_while_ocr_running() {
        let dir = std::env::temp_dir().join("photo-viewer-ocr-step-test");
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png", "c.png"] {
            image::RgbaImage::new(4, 4).save(dir.join(name)).unwrap();
        }

        let mut app = PhotoViewer::new();
        open_and_wait(&mut app, &dir.join("b.png"));
        app.ocr_running = true;
        app.step(1);
        assert_eq!(app.current_name(), "b.png", "识别进行中不应换图");

        app.ocr_running = false;
        app.step(1);
        assert_eq!(app.current_name(), "c.png");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 识别进行中禁止打开其它图片，避免结果与当前图片错位。
    #[test]
    fn open_is_blocked_while_ocr_running() {
        let dir = std::env::temp_dir().join("photo-viewer-ocr-open-test");
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            image::RgbaImage::new(4, 4).save(dir.join(name)).unwrap();
        }

        let mut app = PhotoViewer::new();
        open_and_wait(&mut app, &dir.join("a.png"));
        app.ocr_running = true;
        open_and_wait(&mut app, &dir.join("b.png"));
        assert_eq!(app.current_name(), "a.png", "识别进行中不应切换图片");

        app.ocr_running = false;
        open_and_wait(&mut app, &dir.join("b.png"));
        assert_eq!(app.current_name(), "b.png");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 换图后应从缓存恢复该图的识别结果，缓存未命中则清空。
    #[test]
    fn restore_ocr_boxes_reads_from_cache() {
        let dir = std::env::temp_dir().join("photo-viewer-ocr-restore-test");
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a.png", "b.png"] {
            image::RgbaImage::new(4, 4).save(dir.join(name)).unwrap();
        }

        let mut app = PhotoViewer::new();
        open_and_wait(&mut app, &dir.join("a.png"));
        assert!(app.ocr_boxes.is_empty());

        // 写入缓存后换到 b，再换回 a 应能恢复。
        let boxes = vec![TextBox {
            quad: [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)],
            text: "缓存文字".to_string(),
            score: 0.95,
        }];
        app.ocr_cache.put(dir.join("a.png"), boxes);
        app.step(1);
        assert_eq!(app.current_name(), "b.png");
        assert!(app.ocr_boxes.is_empty(), "b 没有缓存应清空");

        app.step(-1);
        assert_eq!(app.current_name(), "a.png");
        assert_eq!(app.ocr_boxes.len(), 1);
        assert_eq!(app.ocr_boxes[0].text, "缓存文字");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 文本框命中检测：框内点命中，框外点不命中。
    #[test]
    fn ocr_box_at_detects_hit_inside_box() {
        let mut app = PhotoViewer::new();
        app.ocr_boxes = vec![TextBox {
            quad: [(10.0, 10.0), (110.0, 10.0), (110.0, 60.0), (10.0, 60.0)],
            text: "文字".to_string(),
            score: 0.9,
        }];
        // 原图 200x100 显示在 rect(0,0)-(400,200)，即 2 倍缩放。
        let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(400.0, 200.0));
        let original = [200usize, 100usize];

        // 原图 (50, 30) → 显示坐标 (100, 60)，在框内。
        assert_eq!(app.ocr_box_at(egui::pos2(100.0, 60.0), rect, original), Some(0));
        // 显示坐标 (300, 150) 对应原图 (150, 75)，在框外。
        assert_eq!(app.ocr_box_at(egui::pos2(300.0, 150.0), rect, original), None);
    }

    /// 多边形命中检测。
    #[test]
    fn point_in_polygon_identifies_region() {
        let square = [
            egui::pos2(0.0, 0.0),
            egui::pos2(10.0, 0.0),
            egui::pos2(10.0, 10.0),
            egui::pos2(0.0, 10.0),
        ];
        assert!(point_in_polygon(egui::pos2(5.0, 5.0), &square));
        assert!(!point_in_polygon(egui::pos2(15.0, 5.0), &square));
        assert!(!point_in_polygon(egui::pos2(5.0, 15.0), &square));
    }

    /// 造两张测试图片，并返回所在目录。
    fn edit_fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for file in ["a.png", "b.png"] {
            image::RgbaImage::new(8, 4).save(dir.join(file)).unwrap();
        }
        dir
    }

    /// 打开图片并塞入已解码的原图，模拟解码完成后的状态。
    fn opened_app(dir: &Path) -> PhotoViewer {
        let mut app = PhotoViewer::new();
        open_and_wait(&mut app, &dir.join("a.png"));
        app.source = Some(Arc::new(image::RgbaImage::new(8, 4)));
        app.content_size = [8, 4];
        app
    }

    /// 保存编辑会覆盖原图，并复位编辑状态。
    #[test]
    fn save_edits_overwrites_original_file() {
        let dir = edit_fixture("photo-viewer-edit-save-test");
        let mut app = opened_app(&dir);
        app.edit.scale = Some(50);
        app.save_edits();

        let saved = image::open(dir.join("a.png")).unwrap().to_rgba8();
        assert_eq!((saved.width(), saved.height()), (4, 2));
        assert!(!app.edit.is_modified(), "保存后不应再有未保存的修改");
        assert!(app.preview.is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 自动保存关闭时，换图会放弃编辑，原图保持不变。
    #[test]
    fn switching_discards_edits_when_auto_save_is_off() {
        let dir = edit_fixture("photo-viewer-edit-discard-test");
        let mut app = opened_app(&dir);
        app.settings.auto_save_edit = false;
        app.edit.scale = Some(50);
        app.step(1);

        assert_eq!(app.current_name(), "b.png");
        let saved = image::open(dir.join("a.png")).unwrap().to_rgba8();
        assert_eq!((saved.width(), saved.height()), (8, 4), "自动保存关闭时不应改写原图");
        assert!(!app.edit.is_modified());
        assert!(app.preview.is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 自动保存开启时，换图会先把编辑写回原图。
    #[test]
    fn switching_saves_edits_when_auto_save_is_on() {
        let dir = edit_fixture("photo-viewer-edit-autosave-test");
        let mut app = opened_app(&dir);
        app.settings.auto_save_edit = true;
        app.edit.scale = Some(50);
        app.step(1);

        assert_eq!(app.current_name(), "b.png");
        let saved = image::open(dir.join("a.png")).unwrap().to_rgba8();
        assert_eq!((saved.width(), saved.height()), (4, 2), "自动保存开启时应改写原图");
        assert!(!app.edit.is_modified());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 建一个两张 8x4 白图的目录，用于注释相关用例。
    fn anno_fixture(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for file in ["a.png", "b.png"] {
            image::RgbaImage::from_pixel(8, 4, image::Rgba([255, 255, 255, 255]))
                .save(dir.join(file))
                .unwrap();
        }
        dir
    }

    /// 打开图片并塞入已解码的白底原图，模拟解码完成后的状态。
    fn opened_anno_app(dir: &Path) -> PhotoViewer {
        let mut app = PhotoViewer::new();
        open_and_wait(&mut app, &dir.join("a.png"));
        app.source = Some(Arc::new(
            image::RgbaImage::from_pixel(8, 4, image::Rgba([255, 255, 255, 255])),
        ));
        app.content_size = [8, 4];
        app
    }

    /// 一条盖住左上角的红色矩形注释。
    fn sample_annotation() -> annotation::Annotation {
        annotation::Annotation {
            shape: annotation::Shape::Rect,
            color: annotation::Color::Red,
            width_pct: 50.0,
            start: egui::Vec2::new(0.0, 0.0),
            end: egui::Vec2::new(0.5, 0.5),
        }
    }

    /// 手动保存注释会把注释写进原图并清空注释列表。
    #[test]
    fn saving_annotations_writes_them_into_the_original_file() {
        let dir = anno_fixture("photo-viewer-anno-save-test");
        let mut app = opened_anno_app(&dir);
        app.annotations.push(sample_annotation());

        app.save_annotations();
        assert!(app.annotations.is_empty(), "保存后注释列表应清空");
        let saved = image::open(dir.join("a.png")).unwrap().to_rgba8();
        assert_eq!(
            saved.get_pixel(0, 0).0,
            [220, 40, 40, 255],
            "原图应被写入注释"
        );
        assert_eq!(
            saved.get_pixel(7, 3).0,
            [255, 255, 255, 255],
            "未覆盖处应保持原样"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 没有注释时保存给出提示，不改动原图。
    #[test]
    fn saving_without_annotations_reports_nothing_to_save() {
        let dir = anno_fixture("photo-viewer-anno-empty-test");
        let mut app = opened_anno_app(&dir);
        app.save_annotations();
        assert_eq!(app.status.as_deref(), Some("没有需要保存的注释"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 有未保存的注释时换图被推迟，注释保留等待确认。
    #[test]
    fn switching_with_unsaved_annotations_is_deferred() {
        let dir = anno_fixture("photo-viewer-anno-defer-test");
        let mut app = opened_anno_app(&dir);
        app.settings.auto_save_annotation = false;
        app.annotations.push(sample_annotation());

        app.step(1);
        assert_eq!(app.current_name(), "a.png", "有未保存注释时不应直接换图");
        assert!(app.pending_switch.is_some(), "换图请求应被推迟");
        assert_eq!(app.annotations.len(), 1, "推迟期间注释应保留");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 自动保存开启时，换图前把注释写回原图再切换。
    #[test]
    fn switching_auto_saves_annotations_when_enabled() {
        let dir = anno_fixture("photo-viewer-anno-autosave-test");
        let mut app = opened_anno_app(&dir);
        app.settings.auto_save_annotation = true;
        app.annotations.push(sample_annotation());

        app.step(1);
        assert_eq!(app.current_name(), "b.png", "自动保存后应正常换图");
        assert!(app.annotations.is_empty(), "保存后注释列表应清空");
        let saved = image::open(dir.join("a.png")).unwrap().to_rgba8();
        assert_eq!(
            saved.get_pixel(0, 0).0,
            [220, 40, 40, 255],
            "自动保存应把注释写进原图"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 编辑更新显示内容时保留旧纹理，避免画面闪烁。
    #[test]
    fn editing_keeps_old_texture_until_new_one_arrives() {
        let dir = edit_fixture("photo-viewer-edit-texture-test");
        let mut app = opened_app(&dir);
        let ctx = egui::Context::default();
        let pixels = vec![egui::Color32::WHITE; 4];
        app.tex = Some(ctx.load_texture(
            "t",
            egui::ColorImage::new([2, 2], pixels),
            egui::TextureOptions::LINEAR,
        ));
        app.tex_px = [2, 2];

        app.edit.grayscale = true;
        app.refresh_preview();

        // 等后台预览渲染完成，内容换上来的瞬间旧纹理仍要留着。
        for _ in 0..200 {
            app.poll_editor();
            if app.preview.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(app.preview.is_some(), "编辑后应生成预览");
        assert!(app.tex.is_some(), "编辑时应保留旧纹理，避免画面空白");
        assert!(app.texture_stale, "内容换上后纹理应标记为待更新");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 编辑会清空当前图片的识别结果。
    #[test]
    fn editing_clears_ocr_results() {
        let dir = edit_fixture("photo-viewer-edit-ocr-test");
        let mut app = opened_app(&dir);
        app.ocr_boxes = vec![TextBox {
            quad: [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)],
            text: "旧文字".to_string(),
            score: 0.9,
        }];
        app.ocr_cache.put(
            dir.join("a.png"),
            vec![TextBox {
                quad: [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)],
                text: "旧文字".to_string(),
                score: 0.9,
            }],
        );

        app.edit.grayscale = true;
        app.refresh_preview();
        app.clear_ocr();

        assert!(app.ocr_boxes.is_empty(), "编辑后应清空识别结果");
        assert!(app.ocr_cache.get(&dir.join("a.png")).is_none(), "编辑后应清空识别缓存");

        // 预览在后台线程渲染，等结果回来。
        for _ in 0..200 {
            app.poll_editor();
            if app.preview.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(app.preview.is_some(), "编辑后应生成预览");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 轮询后台解码结果，直到条件成立或超时。
    fn wait_until(app: &mut PhotoViewer, timeout: Duration, cond: impl Fn(&PhotoViewer) -> bool) -> bool {
        let ctx = egui::Context::default();
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            app.poll(&ctx);
            if cond(app) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    /// 加载途中继续切换：最终显示的是最后请求的那张图片。
    #[test]
    fn switching_during_load_shows_last_requested_image() {
        let dir = std::env::temp_dir().join("photo-viewer-switch-during-load-test");
        std::fs::create_dir_all(&dir).unwrap();
        // 尺寸各不相同，解码结果能直接看出是哪一张。
        image::RgbaImage::new(10, 10).save(dir.join("a.png")).unwrap();
        image::RgbaImage::new(20, 20).save(dir.join("b.png")).unwrap();
        image::RgbaImage::new(30, 30).save(dir.join("c.png")).unwrap();

        let mut app = PhotoViewer::new();
        open_and_wait(&mut app, &dir.join("a.png"));
        // 第一张还没解码完就继续点下一张。
        app.step(1);
        app.step(1);
        assert_eq!(app.current_name(), "c.png", "连续切换应走到第三张");

        assert!(
            wait_until(&mut app, Duration::from_secs(10), |app| app.source.is_some()),
            "解码结果应在超时前送达"
        );
        assert_eq!(app.content_size, [30, 30], "应显示最后请求的 c.png");
        assert!(!app.load_failed, "正常图片不应标记为加载失败");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 图片解码失败时提示「图片加载失败」。
    #[test]
    fn failed_load_reports_failure() {
        let dir = std::env::temp_dir().join("photo-viewer-load-failed-test");
        std::fs::create_dir_all(&dir).unwrap();
        // 扩展名合法但内容不是图片，解码必然失败。
        std::fs::write(dir.join("bad.png"), b"not an image").unwrap();

        let mut app = PhotoViewer::new();
        open_and_wait(&mut app, &dir.join("bad.png"));

        assert!(
            wait_until(&mut app, Duration::from_secs(10), |app| app.load_failed),
            "解码失败应在超时前被感知"
        );
        assert_eq!(app.status.as_deref(), Some("图片加载失败"));
        assert_eq!(app.empty_hint(), "图片加载失败");
        assert_eq!(app.loading_hint(), "图片加载失败");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 加载提示只说明加载状态，不再出现「正在准备显示」的文案。
    #[test]
    fn loading_hints_avoid_preparing_text() {        let mut app = PhotoViewer::new();
        app.loading = true;
        assert_eq!(app.loading_hint(), "正在加载…");
        assert_eq!(app.empty_hint(), "正在加载…");

        app.loading = false;
        app.load_failed = true;
        assert_eq!(app.loading_hint(), "图片加载失败");
        assert_eq!(app.empty_hint(), "图片加载失败");

        // 源码里也不该再留有这句文案，防止以后又被画回画布。
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/viewer.rs"),
        )
        .unwrap();
        // 只查生产代码，测试模块里的断言文本不算。
        let code = source.split("#[cfg(test)]").next().unwrap_or(&source);
        assert!(
            !code.contains("正在准备显示"),
            "画布不应再显示「正在准备显示」"
        );
    }

    /// 画布实际画出的文字：加载中提示加载，失败时提示「图片加载失败」。
    #[test]
    fn canvas_paints_load_state_hints() {
        let mut app = PhotoViewer::new();
        // 没有图片：给出引导。
        let texts = canvas_texts(&mut app, 800.0, 600.0);
        assert!(
            texts.iter().any(|text| text.contains("拖放图片到窗口")),
            "未打开图片时应提示拖放引导，实际 {texts:?}"
        );

        // 加载中：提示加载状态。
        app.loading = true;
        let texts = canvas_texts(&mut app, 800.0, 600.0);
        assert!(
            texts.iter().any(|text| text == "正在加载…"),
            "加载中应提示正在加载，实际 {texts:?}"
        );

        // 加载失败：提示失败。
        app.loading = false;
        app.load_failed = true;
        let texts = canvas_texts(&mut app, 800.0, 600.0);
        assert!(
            texts.iter().any(|text| text == "图片加载失败"),
            "加载失败应提示图片加载失败，实际 {texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.contains("准备显示")),
            "不应出现准备显示文案，实际 {texts:?}"
        );
    }

    /// 图片已解码、纹理还没生成时：只提示加载状态，不显示准备就绪文案。
    #[test]
    fn canvas_paints_loading_hint_while_texture_pending() {
        let dir = std::env::temp_dir().join("photo-viewer-canvas-hint-test");
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::new(4, 4).save(dir.join("a.png")).unwrap();

        let mut app = PhotoViewer::new();
        open_and_wait(&mut app, &dir.join("a.png"));
        // 模拟解码完成：纹理还在后台生成。
        app.source = Some(Arc::new(image::RgbaImage::new(2000, 1500)));
        app.content_size = [2000, 1500];

        let texts = canvas_texts(&mut app, 800.0, 600.0);
        assert!(
            texts.iter().any(|text| text == "正在加载…"),
            "纹理未就绪时应提示正在加载，实际 {texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.contains("准备显示")),
            "不应出现准备显示文案，实际 {texts:?}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 启动即打开图片：打开调用立刻返回，不等目录扫描。
    #[test]
    fn open_returns_without_waiting_for_directory_scan() {
        let dir = std::env::temp_dir().join("photo-viewer-open-async-test");
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::new(4, 4).save(dir.join("a.png")).unwrap();

        let mut app = PhotoViewer::new();
        app.open(&dir.join("a.png"));
        // 登记完就返回：列表还没建好，但界面已经有加载反馈。
        assert!(app.gallery.is_empty(), "打开不应阻塞在目录扫描上");
        assert!(app.loading, "打开后应提示正在加载");
        assert!(app.source.is_none(), "打开后不应还留着上一张图片");

        // 后台扫描完成后列表就位。
        wait_for_open(&mut app);
        assert_eq!(app.gallery.len(), 1);
        assert_eq!(app.current_name(), "a.png");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 启动加载期间画面上显示「正在加载」。
    #[test]
    fn canvas_shows_loading_hint_while_startup_open_pending() {
        let dir = std::env::temp_dir().join("photo-viewer-startup-hint-test");
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::new(4, 4).save(dir.join("a.png")).unwrap();

        let mut app = PhotoViewer::new();
        // 模拟启动时带上图片参数：窗口先画出来，图片在后台加载。
        app.open(&dir.join("a.png"));
        let texts = canvas_texts(&mut app, 800.0, 600.0);
        assert!(
            texts.iter().any(|text| text == "正在加载…"),
            "启动加载期间画面应显示正在加载，实际 {texts:?}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 打开的文件不存在时提示加载失败。
    #[test]
    fn opening_missing_file_reports_failure() {
        let dir = std::env::temp_dir().join("photo-viewer-open-missing-test");
        std::fs::create_dir_all(&dir).unwrap();

        let mut app = PhotoViewer::new();
        app.open(&dir.join("missing.png"));
        wait_for_open(&mut app);

        assert!(app.load_failed, "文件不存在应标记为加载失败");
        assert_eq!(app.status.as_deref(), Some("图片加载失败"));
        assert!(!app.loading, "失败后不应还在加载中");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
