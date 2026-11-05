# Snapshot 技术方案

2026-11-05。按 Windows 10/11 x64 单平台设计，以托盘空闲时的 CPU、唤醒和内存开销为第一优先级。本方案基于官方 API 文档，性能指标等待原型实测。

## 技术栈

**首选 Rust stable（`x86_64-pc-windows-msvc`）+ `windows` crate + 原生 Win32。** `windows` 提供类型化 Win32、COM 和 WinRT 绑定，可以从 GDI、剪贴板和 WIC 起步，后续沿用同一套绑定接入 Windows Graphics Capture。把 HWND、GDI 对象和 COM 初始化的所有权收在各自的 Windows 适配层。选 Rust 的依据是本机工具链已就绪，且可以用所有权封装原生资源生命周期。Rust 与 C++ 的开销差异需要实测。 [Microsoft windows-rs 绑定选型](https://github.com/microsoft/windows-rs/blob/master/docs/crates/windows.md)

| 方案 | 适用性 |
| --- | --- |
| C++ + 原生 Win32 | 同样能实现单进程、消息驱动、按需抓图，Windows SDK 示例可直接参考。如果 Win32 调试和开发熟练度明显占优，这是同等优先的备选。 [Microsoft GDI 截图示例](https://learn.microsoft.com/en-us/windows/win32/gdi/capturing-an-image) |
| C# + WPF | XAML 和控件适合复杂设置或标注 UI。WPF 包含 CLR、Dispatcher 和保留式渲染系统，常驻基线需要单独量。截屏和全局热键仍可沿用 Win32 API。 [Microsoft WPF 架构](https://learn.microsoft.com/en-us/dotnet/desktop/wpf/advanced/wpf-architecture) |
| Tauri | 适合有明显 Web UI 开发需求时采用。Windows 上创建 WebView 会使用 WebView2，涉及浏览器、渲染器等进程。若以后需要 Web 设置页，可以单独评估按需打开和释放，并统计整个进程树。 [Tauri WebView](https://v2.tauri.app/reference/webview-versions/) · [WebView2 进程模型](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/process-model) |

## 常驻与截图会话

1. **空闲**。一个隐藏的顶层窗口处理托盘与全局热键消息。托盘用 `Shell_NotifyIcon`，热键用 `RegisterHotKey` 接收 `WM_HOTKEY`，由阻塞式 `GetMessage` 等待。处理热键占用和 Explorer 重启后的托盘重建。空闲态只保留消息循环与轻量配置，截图缓冲区、录制会话、GPU 设备、OCR 模型和翻译客户端均按需创建。 [托盘 API](https://learn.microsoft.com/en-us/windows/win32/shell/notification-area) · [热键](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey) · [GetMessage](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getmessage)
2. **触发**。收到热键后先抓取桌面，拿到像素后再显示选区遮罩，防止把遮罩截进画面。选区窗口绘制冻结帧，拖拽时只更新受影响的区域。候选实现是以显示器为单位建遮罩，用物理像素统一选区和裁剪。应用清单声明 `PerMonitorV2`，坐标支持副屏在左侧或上方时的负值。显示布局或 DPI 在会话中变化时取消本次截图，下一次触发重新枚举。 [Microsoft 高 DPI 桌面指南](https://learn.microsoft.com/en-us/windows/win32/hidpi/high-dpi-desktop-application-development-on-windows) · [虚拟屏幕坐标](https://learn.microsoft.com/en-us/windows/win32/gdi/the-virtual-screen)
3. **输出**。冻结帧只裁一次。图片复制用 `CF_DIB` 或 `CF_DIBV5`，PNG 文件用系统 WIC 编码。完成或取消都关闭遮罩并释放会话图像和相关句柄。32 位原始 3840×2160 像素缓冲约 31.6 MiB，每多保留一份就要再付出相近的内存，因此要观察多屏和 4K 的峰值。 [剪贴板格式](https://learn.microsoft.com/en-us/windows/win32/dataxchg/clipboard-formats) · [WIC 内置编解码器](https://learn.microsoft.com/en-us/windows/win32/wic/-wic-about-windows-imaging-codec)

## 采集 API

| API | 采用条件 |
| --- | --- |
| **GDI `BitBlt`** | 首版用 `SRCCOPY \| CAPTUREBLT` 从屏幕 DC 复制到会话内位图，`CAPTUREBLT` 用于纳入叠加的分层窗口。验证浏览器视频、分层窗口、GPU 加速应用、多屏和 HDR 环境中的实际结果。普通 SDR 桌面是首版目标，保护内容、特殊桌面和独占全屏不做兼容承诺。 [BitBlt](https://learn.microsoft.com/en-us/windows/win32/api/wingdi/nf-wingdi-bitblt) |
| **Windows Graphics Capture（WGC）** | 作为窗口截图或 GDI 覆盖不足时的后续方案。`GraphicsCaptureSession` 起于 Windows 10 1803，桌面程序通过 `CreateForMonitor` 或 `CreateForWindow` 直接指定目标要 1903 起，并要检查设备支持。单帧使用时按需启动 D3D11 和帧池，异步拿到首个有效帧后复制像素并关闭，设置超时。边框消除是另外的能力，`IsBorderRequired` 受系统版本限制，且涉及清单能力和用户授权。HDR 需单独验证像素格式及转 SDR 的色调映射。 [会话版本](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.graphicscapturesession) · [桌面互操作](https://learn.microsoft.com/en-us/windows/win32/api/windows.graphics.capture.interop/nn-windows-graphics-capture-interop-igraphicscaptureiteminterop) · [边框权限](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.graphicscapturesession.isborderrequired) · [采集与 HDR](https://learn.microsoft.com/en-us/windows/apps/develop/media-authoring-processing/screen-capture) |
| **DXGI Desktop Duplication** | 需要持续桌面帧时再评估。按显示输出取得 GPU 图像，以及脏矩形、移动矩形和光标信息。还要处理旋转、桌面切换、帧超时和 `DXGI_ERROR_ACCESS_LOST` 后重建。单帧原型先用 GDI 建立基线。 [Desktop Duplication API](https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/desktop-dup-api) |

## 指针自动框选

Snipaste 官网列出“自动检测界面元素区域”，专业版说明还展示滚轮切换父子元素。这些资料描述交互，无法据此确认 Snipaste 用了哪种检测接口。Snapshot 可以分窗口、控件、网页和图像四类来做。 [Snipaste 官网](https://zh.snipaste.com/) · [专业版说明](https://github.com/Snipaste/feedback/wiki/%E4%B8%93%E4%B8%9A%E7%89%88)

| 粒度 | 实现与限制 |
| --- | --- |
| 窗口 | 每次截图会话在遮罩显示前记录可见顶层窗口的 Z 序和矩形。优先用 DWM 的 `DWMWA_EXTENDED_FRAME_BOUNDS` 取可见框，过滤最小化、不可见及 `DWMWA_CLOAKED` 窗口。指针移动时在缓存中选最上层包含该点的矩形。矩形近似会覆盖圆角或透明区域，拖拽可以覆盖建议框。 [DWM 窗口属性](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwmwindowattribute) · [Win32 Z 序](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getwindow) |
| 控件 | Windows UI Automation（UIA）按桌面物理坐标命中元素，`ElementFromPointBuildCache` 可以随命中取回边界等属性。Chromium 支持 Windows UIA，浏览器暴露的无障碍树由 DOM、CSS 和 ARIA 等信息派生，部分布局节点会省略，结果依赖目标应用的 provider。画布、游戏和自绘界面可能只能取得外层矩形。查询放在会话内按需启动的 COM MTA 工作线程，合并指针请求并丢弃过期结果。遮罩显示后全局命中可能返回遮罩本身，优先验证按目标窗口取候选并在内存命中，或只在遮罩显示前全局命中。 [UIA 命中与缓存](https://learn.microsoft.com/en-us/windows/win32/api/uiautomationclient/nf-uiautomationclient-iuiautomation-elementfrompointbuildcache) · [UIA 线程](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-threading) · [Chromium 无障碍架构](https://chromium.googlesource.com/chromium/src/+/main/docs/accessibility/overview.md) |
| DOM 元素 | 如需框到网页中的具体 HTML 元素，可让授权的浏览器扩展在页面内用 `document.elementFromPoint()` 做命中，用 `getBoundingClientRect()` 得到视口 CSS 像素边界。接入桌面截图还要处理 iframe、页面缩放、浏览器内容区原点和屏幕坐标转换。扩展的 `activeTab` 授权来自用户触发扩展，受限页面也有访问限制。 [CSSOM View 规范](https://drafts.csswg.org/cssom-view/) · [Chrome activeTab 权限](https://developer.chrome.com/docs/extensions/develop/concepts/activeTab) |
| 图像区域 | 目标未暴露控件时，可以从已冻结的像素中提取边缘或轮廓，在指针邻域推断矩形。它能处理可见边框，低对比度、文字纹理、圆角和无边框布局会导致歧义。先以真实截图测误框，再决定是否引入。 [OpenCV Canny 边缘检测](https://docs.opencv.org/4.x/da/d22/tutorial_py_canny.html) |

原型冻结前记录窗口层级和可见矩形，随后在截图会话内按指针位置命中。UIA 工作线程从目标窗口的句柄进入，逐层查询缓存了矩形和可见性的子节点，优先保留指针下较小的候选区域。缺少可用元素时保留窗口范围。单击锁定悬停矩形，拖拽可建立自由选区，右键回到自动预选。仍需覆盖 Chrome/Edge、资源管理器、自绘程序、遮挡窗口和双屏混合 DPI。

## 首版范围和后续功能

首版做托盘启动与退出、全局热键、冻结画面矩形选区、窗口和控件吸附、选区移动及八点缩放、Esc 取消、复制到剪贴板和保存 PNG。使用双屏混合 DPI 验证物理像素裁剪，包括跨屏选区。贴图、标注、滚动长截图和 OCR/翻译放到截图链路稳定后。

- **滚动长截图**。进入模式后固定视口，允许用户手动滚动，按需取帧并匹配重叠区域。对重复纹理、固定页头和低置信度接缝保留预览与重试，限制会话最长输出和内存峰值。自动滚动可作为增强，在目标 UI Automation provider 实现 `ScrollPattern` 时尝试。 [Microsoft UI Automation 控件模式](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-implementinguiautocontrolpatterns)
- **OCR 和翻译**。用选区结果显式触发，既可单独复制原文，也可把文本交给翻译接口。优先评估 PaddleOCR 的本地推理，模型按需加载，并验证首次初始化延迟与调用后内存回落。若主要追求接入速度，可评估百度图片翻译的单请求路径，用户选择该路径时才上传截图。 [PaddleOCR 本地推理文档](https://github.com/PaddlePaddle/PaddleOCR/blob/main/docs/version3.x/pipeline_usage/OCR.en.md) · [百度图片翻译](https://fanyi-api.baidu.com/product/231)

## 验证方法

| 场景 | 测法 |
| --- | --- |
| 空闲基线 | 同一台机器用 Release 版本，记录系统版本、分辨率、缩放和显示器布局。冷启动后静置 60 秒，再采样 10 分钟。记录进程树 CPU 时间增量、Private Bytes（私有提交）、Working Set（驻留内存）和线程数。用 WPR/WPA 看是否存在本程序造成的周期性唤醒、磁盘或网络 I/O。 [Microsoft 进程内存统计](https://learn.microsoft.com/en-us/windows/win32/psapi/process-memory-usage-information) · [WPR/WPA](https://learn.microsoft.com/en-us/windows-hardware/test/wpt/introduction-to-wpr) |
| 首帧延迟 | 在 `WM_HOTKEY` 处理入口、抓图完成、遮罩首绘分别打 `QueryPerformanceCounter` 时间戳，按 1080p、4K 和双屏各采集 30 次并报告中位数、P95。分别记录进程首次截图和后续截图，编码或写盘单独计时。 [Microsoft 高精度计时 API](https://learn.microsoft.com/en-us/windows/win32/api/profileapi/nf-profileapi-queryperformancecounter) |
| 资源回收 | 重复 50 次截取、复制、保存和取消，记录每种路径的峰值，以及结束 60 秒后的空闲数据。检查 GDI 对象数、句柄数和 Private Bytes 是否随会话数持续增长，同时验证无残留遮罩。 [Microsoft GetGuiResources](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getguiresources) |
| 正确性 | 用带像素网格和色块的测试画面验证 100%/150% 混合缩放、负坐标屏、竖屏和跨屏，核对 PNG 尺寸、像素位置及剪贴板粘贴结果。另测连续按热键、快捷键冲突、Explorer 重启以及会话中切换显示配置。 |

第一轮先跑出基线，再定数值预算。无常驻采集、无应用自设轮询、重复截图无资源持续增长是首版验收条件。若空闲指标接近，比较首次截图延迟和实际维护成本。
