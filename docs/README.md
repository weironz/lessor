# docs

真实客户端打出来的问题，以及排查它们的办法。写下来是因为这些结论在
代码里只剩一行注释，而当初找到它们花的时间远不止一行。

## PXE / 网络引导

- [**PXE 客户端识别**](pxe-client-identification.md) ——
  lessor 怎么从 option 60 认出网络引导中的机器，架构码怎么读，
  同一台机器在固件阶段和操作系统阶段为什么是两条记录。
- [**应答的源端口必须是 67**](pxe-source-port.md) ——
  服务端日志一行行"已应答"，客户端却一直重发 DISCOVER。
  普通 DHCP 客户端不校验源端口，PXE 固件校验。
- [**option 60 与 option 43：要么都给，要么都不给**](pxe-option-60-and-43.md) ——
  地址拿到了，TFTP 一个请求都不发，直接掉回启动菜单。
- [**怎么对着真固件排查 PXE**](debugging-pxe.md) ——
  分清"包没到"和"包到了但不认"、看固件控制台、逐字节摊开报文、
  找一个已知能用的参照物。附几个把人带偏的坑。

## 别处

- 权限相关（为什么 `lessord` 不需要管理员 / root）见
  [README 的"不需要特权"一节](../README.md#不需要特权)
- 手工验证脚本见 [scripts/README.md](../scripts/README.md)
- Linux capability 的实测依据见 [docker/setcaptest.sh](../docker/setcaptest.sh)
