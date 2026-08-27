//! 前端资源。构建产物直接打进二进制 —— 部署时只有一个可执行文件。

use rust_embed::Embed;

/// `ui/dist` 是 Vite 的输出目录，路径相对于本 crate。
///
/// **这个目录必须存在，否则整个 crate 编译不过** —— rust-embed 在编译期
/// 就要读它，报的是 `folder ... does not exist`。所以构建顺序是固定的：
/// 先 `bun run build`（在 ui/ 下），再 `cargo build`。CI 里也是这个顺序。
/// （目录存在但为空是可以的，那种情况下界面路由会回一份纯文本说明。）
#[derive(Embed)]
#[folder = "../../ui/dist"]
pub struct Assets;
