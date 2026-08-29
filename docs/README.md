# docs

真实客户端打出来的问题，以及排查它们的办法。写下来是因为这些结论在
代码里只剩一行注释，而当初找到它们花的时间远不止一行。

## 先看这个

- [**设计方案与开发计划**](design-and-plan.md) —— 为什么要造（竞品逐个不行在哪）、
  两个关键决策的评审记录（含翻盘条件）、安全红线、与 MAAS 的分工、
  里程碑 M0–M9 与验收判据。
- [**架构**](architecture.md) —— 技术栈、分层、一个报文的旅程、平台差异、
  特权边界怎么切、为什么前后端一体、测试策略与欠账。

## 桌面端

- [**自动更新**](desktop-update.md) —— "关于"里的检查更新怎么工作、
  为什么更新包必须签名（和 Windows 代码签名证书是两回事）、
  以及首次启用要生成的密钥对。更新会停掉本机 lessord，但不碰外部实例。

## 现场

- [**现场 runbook**](field-runbook.md) —— 带一台笔记本去机房，从插网线到走人。
  面向现场那个人：每步写清"看到什么算成了"和"没看到怎么办"，
  以及需要管理员的三处分别是什么（跑 lessord 本身不在其中）。

## PXE / 网络引导

- [**引导客户端识别**](pxe-client-identification.md) ——
  从 option 60 / option 77 分出 PXE 固件、UEFI HTTP Boot、iPXE 三类，
  各发各的引导目标；架构码怎么读；同一台机器在固件阶段和操作系统阶段
  为什么是两条记录。三类都有真固件实测数据。
- [**应答的源端口必须是 67**](pxe-source-port.md) ——
  服务端日志一行行"已应答"，客户端却一直重发 DISCOVER。
  普通 DHCP 客户端不校验源端口，PXE 固件校验。
- [**option 60 与 option 43：要么都给，要么都不给**](pxe-option-60-and-43.md) ——
  地址拿到了，TFTP 一个请求都不发，直接掉回启动菜单。
- [**怎么对着真固件排查 PXE**](debugging-pxe.md) ——
  分清"包没到"和"包到了但不认"、看固件控制台、逐字节摊开报文、
  找一个已知能用的参照物。另附 UEFI HTTP Boot 和 iPXE 链式引导的搭建步骤，
  以及测试台本身的坑（网卡型号、EFI NVRAM）—— 后者和 DHCP 无关，
  却最容易被误判成 DHCP 的问题。

## 跨网段

- [**DHCP 中继**](dhcp-relay.md) —— DHCP 首次交互是二层广播，跨网段靠
  路由器上的 `ip helper-address`。怎么配 `viaRelay`、option 54 下发的是谁，
  以及一个真出过问题的坑：**续租不经过中继**（没有 `giaddr` 只有 `ciaddr`），
  判断网段时漏掉这一层会把正常工作的客户端 NAK 掉。

## 与别人共处一个网段

- [**冲突检测**](dhcp-conflict-detection.md) ——
  不发已被静态占用的地址（探测为什么不能放在握手路径上），
  以及探测同网段其他 DHCP 服务器时的一个坑：**必须绑 68 端口收**，
  绑临时端口一个应答都收不到，然后稳定地报"没有其他 DHCP"。
  一条安全检查给出假的全清信号比不做更糟。附 MAAS 网段旁挂实测。

## 别处

- 权限相关（为什么 `lessord` 不需要管理员 / root）见
  [README 的"不需要特权"一节](../README.md#不需要特权)
- 手工验证脚本见 [scripts/README.md](../scripts/README.md)
- Linux capability 的实测依据见 [docker/setcaptest.sh](../docker/setcaptest.sh)
