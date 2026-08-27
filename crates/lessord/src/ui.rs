//! 前端资源。构建产物直接打进二进制 —— 部署时只有一个可执行文件。

use rust_embed::Embed;

/// `ui/dist` 是 Vite 的输出目录，路径相对于本 crate。
/// 没构建过前端时这里是空的，服务端会退回到一份纯文本说明，而不是 500。
#[derive(Embed)]
#[folder = "../../ui/dist"]
pub struct Assets;
