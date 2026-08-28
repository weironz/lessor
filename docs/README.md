# docs

真实客户端打出来的问题，以及排查它们的办法。写下来是因为这些结论在
代码里只剩一行注释，而当初找到它们花的时间远不止一行。

## 先看这个

- [**设计方案与开发计划**](design-and-plan.md) —— 为什么要造（竞品逐个不行在哪）、
  两个关键决策的评审记录（含翻盘条件）、安全红线、与 MAAS 的分工、
  里程碑 M0–M9 与验收判据。
- [**架构**](architecture.md) —— 技术栈、分层、一个报文的旅程、平台差异、
  特权边界怎么切、为什么前后端一体、测试策略与欠账。

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

## 别处

- 权限相关（为什么 `lessord` 不需要管理员 / root）见
  [README 的"不需要特权"一节](../README.md#不需要特权)
- 手工验证脚本见 [scripts/README.md](../scripts/README.md)
- Linux capability 的实测依据见 [docker/setcaptest.sh](../docker/setcaptest.sh)
