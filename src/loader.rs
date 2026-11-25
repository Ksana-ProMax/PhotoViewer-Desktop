//! 后台解码与缩放：工作线程持有解码结果，按视口需求缩放后回传；另有预取线程静默缓存前后图片。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use image::RgbaImage;
use image::imageops::FilterType;


/// 默认缓存容量。
const DEFAULT_CACHE_CAP: usize = 5;

/// 发送给工作线程的任务。
///
/// `version` 是显示内容的版本号：每次换图或替换显示内容都会前进，
/// 用于丢弃基于旧内容生成的缩放结果。
pub enum Job {
    /// 解码指定图片。
    Load {
        token: u64,
        version: u64,
        path: PathBuf,
    },
    /// 把已解码图片缩放到指定像素尺寸。
    Resize {
        token: u64,
        version: u64,
        px: [usize; 2],
    },
    /// 预取相邻图片，`focus` 为触发时的当前序号。
    Prefetch { focus: usize, path: PathBuf },
    /// 替换当前图片（编辑预览），之后的缩放都基于新图片。
    Replace {
        token: u64,
        version: u64,
        image: Arc<RgbaImage>,
    },
}

/// 工作线程回传的结果。
pub enum Msg {
    /// 解码完成，失败时携带可展示的错误信息。
    Decoded {
        token: u64,
        result: Result<Arc<RgbaImage>, String>,
    },
    /// 缩放完成。
    Resized {
        token: u64,
        version: u64,
        px: [usize; 2],
        rgba: RgbaImage,
    },
}

/// 已解码图片的 LRU 缓存，切换图片时直接命中。
pub struct ImageCache {
    inner: Mutex<CacheInner>,
}

struct CacheInner {
    map: HashMap<PathBuf, Arc<RgbaImage>>,
    /// 由旧到新的使用顺序。
    order: Vec<PathBuf>,
    cap: usize,
}

impl ImageCache {
    pub fn new(cap: usize) -> Self {
        Self {
            inner: Mutex::new(CacheInner {
                map: HashMap::new(),
                order: Vec::new(),
                cap,
            }),
        }
    }

    /// 命中缓存时返回图片并刷新使用顺序。
    pub fn get(&self, path: &Path) -> Option<Arc<RgbaImage>> {
        let mut inner = self.lock();
        let image = inner.map.get(path)?.clone();
        inner.touch(path);
        Some(image)
    }

    /// 写入缓存，超出容量时淘汰最久未使用的图片。
    pub fn put(&self, path: PathBuf, image: Arc<RgbaImage>) {
        let mut inner = self.lock();
        if inner.map.insert(path.clone(), image).is_none() {
            inner.order.push(path);
        } else {
            inner.touch(&path);
        }
        while inner.order.len() > inner.cap {
            let oldest = inner.order.remove(0);
            inner.map.remove(&oldest);
        }
    }

    /// 调整容量，超出部分立即淘汰。
    pub fn set_cap(&self, cap: usize) {
        let mut inner = self.lock();
        inner.cap = cap;
        while inner.order.len() > inner.cap {
            let oldest = inner.order.remove(0);
            inner.map.remove(&oldest);
        }
    }

    /// 删除缓存中的图片，文件被改写后用于强制重新解码。
    pub fn remove(&self, path: &Path) {
        let mut inner = self.lock();
        inner.map.remove(path);
        inner.order.retain(|p| p != path);
    }

    /// 当前缓存的图片数量。
    pub fn len(&self) -> usize {
        self.lock().map.len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheInner> {
        self.inner.lock().unwrap_or_else(|err| err.into_inner())
    }
}

impl CacheInner {
    /// 把路径移到使用顺序末尾。
    fn touch(&mut self, path: &Path) {
        if let Some(pos) = self.order.iter().position(|p| p == path) {
            let path = self.order.remove(pos);
            self.order.push(path);
        }
    }
}

/// 后台加载器：解码、缩放与相邻图片预取。
pub struct Loader {
    jobs: Sender<Job>,
    /// 后台线程回传的结果。
    pub msgs: Receiver<Msg>,
    cache: Arc<ImageCache>,
    /// 当前关注的图片序号，预取线程据此丢弃过期任务。
    focus: Arc<AtomicUsize>,
    /// 最近一次提交的加载令牌：更早的任务直接跳过，只解码用户真正要看的图片。
    latest: Arc<AtomicU64>,
}

impl Loader {
    /// 启动解码线程与预取线程。
    pub fn spawn() -> Self {
        let (job_tx, job_rx) = channel::<Job>();
        let (msg_tx, msg_rx) = channel::<Msg>();
        let cache = Arc::new(ImageCache::new(DEFAULT_CACHE_CAP));
        let focus = Arc::new(AtomicUsize::new(0));
        let latest = Arc::new(AtomicU64::new(0));

        let (prefetch_tx, prefetch_rx) = channel::<(usize, PathBuf)>();
        {
            let cache = cache.clone();
            let focus = focus.clone();
            std::thread::spawn(move || {
                while let Ok((want, path)) = prefetch_rx.recv() {
                    // 焦点已经移动，说明这批预取已过期。
                    if want != focus.load(Ordering::Relaxed) {
                        continue;
                    }
                    if cache.get(&path).is_some() {
                        continue;
                    }
                    let rgba = {
                        perf_scope!(format!("预取解码 {}", file_label(&path)));
                        decode(&path)
                    };
                    match rgba {
                        Ok(rgba) => {
                            let size = (rgba.width(), rgba.height());
                            cache.put(path.clone(), Arc::new(rgba));
                            eprintln!("[prefetch] {:?} {size:?}", path);
                        }
                        Err(err) => eprintln!("[prefetch] {:?} failed: {err}", path),
                    }
                }
            });
        }

        let worker_cache = cache.clone();
        let worker_latest = latest.clone();
        std::thread::spawn(move || {
            // 当前显示内容：令牌 + 版本 + 图片。版本用于丢弃过期内容的结果。
            let mut current: Option<(u64, u64, Arc<RgbaImage>)> = None;
            while let Ok(job) = job_rx.recv() {
                match job {
                    Job::Load {
                        token,
                        version,
                        path,
                    } => {
                        // 期间用户又切了图，这个任务已经过期，不必解码。
                        if token != worker_latest.load(Ordering::Acquire) {
                            eprintln!("[load] 跳过过期任务 token={token}");
                            continue;
                        }
                        let image = match worker_cache.get(&path) {
                            Some(image) => {
                                eprintln!("[cache] hit {:?}", path);
                                image
                            }
                            None => {
                                let decoded = {
                                    perf_scope!(format!("解码 {}", file_label(&path)));
                                    decode(&path)
                                };
                                match decoded {
                                    Ok(rgba) => {
                                        let image = Arc::new(rgba);
                                        worker_cache.put(path.clone(), image.clone());
                                        image
                                    }
                                    Err(err) => {
                                        eprintln!("decode {:?} failed: {err}", path);
                                        // 失败信息只对最新一次加载有意义：过期的不回传，也不动当前内容。
                                        if token == worker_latest.load(Ordering::Acquire) {
                                            current = None;
                                            let _ = msg_tx.send(Msg::Decoded {
                                                token,
                                                result: Err(err),
                                            });
                                        }
                                        continue;
                                    }
                                }
                            }
                        };
                        // 解码期间用户又切了图，结果直接丢弃，避免显示过期的图片。
                        if token != worker_latest.load(Ordering::Acquire) {
                            eprintln!("[load] 丢弃过期结果 token={token}");
                            continue;
                        }
                        // 只有被接受的结果才作为后续缩放的源。
                        current = Some((token, version, image.clone()));
                        let _ = msg_tx.send(Msg::Decoded {
                            token,
                            result: Ok(image),
                        });
                    }
                    Job::Resize { token, version, px } => {
                        let Some((loaded_token, loaded_version, src)) = &current else {
                            continue;
                        };
                        // 令牌或版本不一致，说明内容已经换过，结果作废。
                        if *loaded_token != token || *loaded_version != version {
                            continue;
                        }
                        eprintln!(
                            "[scale] {}x{} -> {}x{} {}",
                            src.width(),
                            src.height(),
                            px[0],
                            px[1],
                            scale_filter_name(src, px)
                        );
                        let resized = {
                            perf_scope!(format!(
                                "缩放 {}x{} {}",
                                px[0],
                                px[1],
                                scale_filter_name(src, px)
                            ));
                            scale_image(src, px)
                        };
                        let _ = msg_tx.send(Msg::Resized {
                            token,
                            version,
                            px,
                            rgba: resized,
                        });
                    }
                    Job::Prefetch { focus, path } => {
                        let _ = prefetch_tx.send((focus, path));
                    }
                    Job::Replace {
                        token,
                        version,
                        image,
                    } => {
                        // 换掉当前图片，后续缩放任务都以它为源。
                        current = Some((token, version, image));
                    }
                }
            }
        });

        Self {
            jobs: job_tx,
            msgs: msg_rx,
            cache,
            focus,
            latest,
        }
    }

    /// 提交解码任务，同时把令牌登记为最新一次加载。
    pub fn load(&self, token: u64, version: u64, path: PathBuf) {
        self.latest.store(token, Ordering::Release);
        let _ = self.jobs.send(Job::Load {
            token,
            version,
            path,
        });
    }

    /// 提交缩放任务。
    pub fn resize(&self, token: u64, version: u64, px: [usize; 2]) {
        let _ = self.jobs.send(Job::Resize { token, version, px });
    }

    /// 用新图片替换当前图片，缩放任务随之以新图片为源。
    pub fn replace(&self, token: u64, version: u64, image: Arc<RgbaImage>) {
        let _ = self.jobs.send(Job::Replace {
            token,
            version,
            image,
        });
    }

    /// 让缓存中的某张图片失效：文件被改写后重新解码。
    pub fn invalidate(&self, path: &Path) {
        self.cache.remove(path);
        eprintln!("[cache] invalidate {:?}", path);
    }

    /// 标记当前图片序号，使过期的预取任务失效。
    pub fn set_focus(&self, index: usize) {
        self.focus.store(index, Ordering::Relaxed);
    }

    /// 请求静默预取一张图片。
    pub fn prefetch(&self, focus: usize, path: PathBuf) {
        let _ = self.jobs.send(Job::Prefetch { focus, path });
    }

    /// 调整缓存容量。
    pub fn set_cache_cap(&self, cap: usize) {
        self.cache.set_cap(cap);
    }

    /// 当前缓存的图片数量。
    pub fn cached_count(&self) -> usize {
        self.cache.len()
    }
}

fn decode(path: &PathBuf) -> Result<RgbaImage, String> {
    let reader = image::ImageReader::open(path)
        .map_err(|err| format!("无法打开文件: {err}"))?
        .with_guessed_format()
        .map_err(|err| format!("无法识别图片格式: {err}"))?;
    reader
        .decode()
        .map(|img| img.to_rgba8())
        .map_err(|err| format!("解码失败: {err}"))
}

/// 缩到比原图小才用盒式平均：尺寸不变或放大都不适用。
fn is_downscale(src: &RgbaImage, px: [usize; 2]) -> bool {
    px[0] < src.width() as usize && px[1] < src.height() as usize
}

/// 缩放方式的名字，写进日志便于确认实际走了哪条路径。
fn scale_filter_name(src: &RgbaImage, px: [usize; 2]) -> &'static str {
    if is_downscale(src, px) {
        "盒式"
    } else {
        "CatmullRom"
    }
}

/// 缩放到目标像素：缩小时用盒式平均（不混叠且快得多），放大时保留 CatmullRom。
fn scale_image(src: &RgbaImage, px: [usize; 2]) -> RgbaImage {
    let (width, height) = (px[0] as u32, px[1] as u32);
    if is_downscale(src, px) {
        image::imageops::thumbnail(src, width, height)
    } else {
        image::imageops::resize(src, width, height, FilterType::CatmullRom)
    }
}

/// 文件名，用于日志；取不到时退回完整路径。
#[cfg(debug_assertions)]
fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn decode_and_resize_roundtrip() {
        let dir = std::env::temp_dir().join("photo-viewer-loader-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.png");
        image::RgbaImage::new(40, 20).save(&path).unwrap();

        let loader = Loader::spawn();
        loader.load(1, 1, path);
        let decoded = loader.msgs.recv_timeout(Duration::from_secs(10)).unwrap();
        let Msg::Decoded {
            token,
            result: Ok(rgba),
        } = decoded
        else {
            panic!("解码失败");
        };
        assert_eq!(token, 1);
        assert_eq!((rgba.width(), rgba.height()), (40, 20));

        loader.resize(1, 1, [10, 5]);
        let Msg::Resized { px, rgba, .. } = loader.msgs.recv_timeout(Duration::from_secs(10)).unwrap() else {
            panic!("缩放失败");
        };
        assert_eq!(px, [10, 5]);
        assert_eq!((rgba.width(), rgba.height()), (10, 5));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 内容版本前进后，基于旧版本的缩放结果不会产出。
    #[test]
    fn resize_is_dropped_when_content_version_moved_on() {
        let dir = std::env::temp_dir().join("photo-viewer-loader-version-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.png");
        image::RgbaImage::new(40, 20).save(&path).unwrap();

        let loader = Loader::spawn();
        loader.load(1, 1, path);
        let _ = loader.msgs.recv_timeout(Duration::from_secs(10)).unwrap();

        // 换成了新内容，旧版本的缩放任务应被丢弃。
        loader.replace(1, 2, Arc::new(image::RgbaImage::new(8, 4)));
        loader.resize(1, 1, [10, 5]);
        assert!(
            loader.msgs.recv_timeout(Duration::from_millis(300)).is_err(),
            "过期版本的缩放结果不应返回"
        );

        // 新版本的缩放正常返回。
        loader.resize(1, 2, [4, 2]);
        let Msg::Resized { px, rgba, .. } = loader.msgs.recv_timeout(Duration::from_secs(10)).unwrap() else {
            panic!("缩放失败");
        };
        assert_eq!(px, [4, 2]);
        assert_eq!((rgba.width(), rgba.height()), (4, 2));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cache_hits_and_evicts_oldest() {
        let cache = ImageCache::new(2);
        let a = PathBuf::from("a.png");
        let b = PathBuf::from("b.png");
        let c = PathBuf::from("c.png");
        let image = Arc::new(image::RgbaImage::new(2, 2));

        cache.put(a.clone(), image.clone());
        cache.put(b.clone(), image.clone());
        assert!(cache.get(&a).is_some());
        cache.put(c.clone(), image.clone());

        // a 被访问过，最先淘汰的是 b。
        assert!(cache.get(&a).is_some());
        assert!(cache.get(&b).is_none());
        assert!(cache.get(&c).is_some());
        assert_eq!(cache.len(), 2);
    }

    /// 期间又提交了新的加载任务：过期任务不解码，结果也不回传。
    #[test]
    fn stale_load_is_skipped() {
        let dir = std::env::temp_dir().join("photo-viewer-loader-stale-test");
        std::fs::create_dir_all(&dir).unwrap();
        let stale = dir.join("stale.png");
        let fresh = dir.join("fresh.png");
        image::RgbaImage::new(8, 8).save(&stale).unwrap();
        image::RgbaImage::new(8, 8).save(&fresh).unwrap();

        let loader = Loader::spawn();
        // 先让工作线程忙着缩放一张大图，把后面的加载任务压在队列里，
        // 这样两个加载任务都提交完毕，过期判断才是确定的。
        loader.replace(0, 1, Arc::new(image::RgbaImage::new(2000, 2000)));
        loader.resize(0, 1, [2000, 2001]);
        loader.load(1, 2, stale.clone());
        loader.load(2, 3, fresh.clone());

        // 只应回传最新一次加载的结果。
        let mut token = 0;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match loader.msgs.recv_timeout(Duration::from_millis(200)) {
                Ok(Msg::Decoded { token: t, .. }) => {
                    token = t;
                    break;
                }
                Ok(_) => continue,
                Err(_) => continue,
            }
        }
        assert_eq!(token, 2, "只应回传最新一次加载的解码结果");

        // 过期任务没有解码，不该留在缓存里。
        assert!(loader.cached_count() <= 1, "过期图片不应被解码缓存");
        assert!(
            loader.msgs.recv_timeout(Duration::from_millis(300)).is_err(),
            "过期任务不应再产生结果"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 缩小走盒式平均，尺寸不变或放大保留 CatmullRom。
    #[test]
    fn scale_filter_switches_on_direction() {
        let src = image::RgbaImage::new(100, 80);
        assert!(is_downscale(&src, [50, 40]), "两轴都变小应算缩小");
        assert!(!is_downscale(&src, [100, 80]), "同尺寸不算缩小");
        assert!(!is_downscale(&src, [200, 160]), "放大不算缩小");
        assert!(!is_downscale(&src, [50, 160]), "单轴放大不适用盒式");
        assert_eq!(scale_filter_name(&src, [50, 40]), "盒式");
        assert_eq!(scale_filter_name(&src, [200, 160]), "CatmullRom");
    }

    /// 缩放结果尺寸与请求一致，颜色取自原图。
    #[test]
    fn scale_image_keeps_requested_size() {
        let src = image::RgbaImage::from_pixel(40, 30, image::Rgba([10, 20, 30, 255]));
        let scaled = scale_image(&src, [10, 8]);
        assert_eq!((scaled.width(), scaled.height()), (10, 8));
        assert_eq!(scaled.get_pixel(0, 0).0, [10, 20, 30, 255]);
    }
}
