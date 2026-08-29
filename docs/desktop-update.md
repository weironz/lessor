# 桌面端自动更新

界面右上角的版本号是"关于"的入口，里面有检查更新。发现新版本 → 确认 →
停掉本机 lessord → 下载 → 安装 → 重启。

## 为什么必须签名

更新这件事的本质是**下载一个文件然后执行它**。如果不校验来源，那么谁能
替换掉发布物（GitHub 账号被盗、CDN 被投毒、中间人），谁就能在所有装了
lessor 的客户机器上执行代码 —— 而 lessor 跑的是 DHCP，位置相当敏感。

所以更新包用 minisign 签名，桌面端内置公钥校验，对不上就拒装。

**这和 Windows 代码签名证书是两回事**：

| | Tauri 更新签名 | Windows 代码签名证书 |
| --- | --- | --- |
| 作用 | 确认更新包是我们发的 | 让 SmartScreen 不拦 |
| 花钱 | 不花 | 要花，[已决定不买](../ROADMAP.md#已否决) |
| 缺了会怎样 | **自动更新无法使用** | 首次安装被 SmartScreen 拦一道 |

## 一次性准备（需要仓库管理员做）

私钥不能进仓库，也不该经我的手 —— 自己生成、自己填进 Secrets。

### 1. 生成密钥对

```bash
cd ui && bunx tauri signer generate -w ~/.tauri/lessor.key
```

会让你设一个密码（可以留空，但建议设）。输出里有两样东西：

- **私钥**：写在 `~/.tauri/lessor.key`，同时打印在屏幕上。**不要提交、不要
  贴进聊天记录**。丢了就再也发不出能被老版本接受的更新（只能让用户手工重装）。
- **公钥**：`~/.tauri/lessor.key.pub` 里那串。

### 2. 私钥进 GitHub Secrets

仓库 → Settings → Secrets and variables → Actions，新增：

| 名字 | 值 |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | `~/.tauri/lessor.key` 的**全部内容** |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | 上一步设的密码（没设就留空） |

没配的话发布流水线会**在打包那一步直接失败**并说明原因，而不是悄悄产出一个
装不上的更新包。

### 3. 公钥进配置

把 `ui/src-tauri/tauri.conf.json` 里的 `plugins.updater.pubkey` 从
`PUBKEY_PLACEHOLDER` 换成公钥内容，提交。

公钥是**编译进桌面端**的 —— 也就是说换密钥对之后，只有装了新版本的用户才
认新签名。所以这件事最好一次做对。

## 发一版之后会多出什么

Release 里除了原来那些，会多两个文件：

- `lessor_<版本>_x64-setup.exe.sig` —— 安装程序的签名
- `latest.json` —— 更新清单，桌面端就是拉它来发现新版本的

桌面端查的地址固定是
`https://github.com/weironz/lessor/releases/latest/download/latest.json`，
每次发版覆盖，老版本据此知道有新的。

## 更新时会停掉 lessord

**只停桌面端自己拉起的那个。** 界面上确认更新之后：

1. 停掉本机 lessord —— Windows 上安装程序要覆盖 `lessord.exe`，它正在跑
   就装不上去。不停的话装出来会是"新外壳 + 旧服务"，比装失败更难查。
2. 下载、校验签名、安装、重启。

如果界面连的是**外部实例**（注册成系统服务的、或者别人手工起的、或者
`LESSOR_URL` 指向别的机器），桌面端**不会去动它** —— 悄悄杀掉别人的 DHCP
服务器，后果比更新失败严重得多。这种情况下界面会照实说明。

界面上在确认前会提醒：正有机器在取地址的话，等它们装完再更新。

## 只在桌面端可用

同一份界面在浏览器里打开时（比如 docker 部署，或者直接访问 lessord 的
HTTP 端口），"关于"里不会出现检查更新 —— 那里没有"程序"可更新，
按你部署 lessord 的方式升级即可。
