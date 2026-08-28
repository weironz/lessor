# 路线图

里程碑制，不排日历。**这里是开发计划的唯一事实来源** ——
做完的条目移入 [ROADMAP-done.md](ROADMAP-done.md)（带完成日期与验收证据）。
每条决策的"为什么"在 [docs/design-and-plan.md](docs/design-and-plan.md)。

两种运行形态：**现场**（field，笔记本临时用）与**常驻**（resident，长期服务）。
标注在各条目上。

## M5 · 租约持久化（共用；常驻默认开）

- sqlite 后端过 `LeaseStore` 全部 trait 测试；现场形态仍默认 ephemeral
- **关键契约（唯一事后追改贵的点）**：分配语义定成**原子占位** ——
  `try_claim(scope, ip, client) → 成功 | 已被占`，循环换候选。
  这是 v1.0 共享 PG 多实例 HA 的前提，内存 / sqlite / PG 共用一份契约

**验收**：kill -9 重启后同客户端 REQUEST 拿回原 IP、不遭 NAK；
损坏的 db 文件启动时明确报错拒绝，而非静默清空。

## M6 · 常驻化（常驻）

- systemd unit + Windows 服务注册；`--config` 一等公民 + 平滑重载
- 结构化日志；`/metrics`（Prometheus 文本格式）

**验收**：注册为系统服务后开机自启、崩溃自拉起；周级连续运行无泄漏无重启；
配置变更全部在线生效。

## M7 · 冲突检测与共存（共用）

- OFFER 前对候选地址做 ARP/UDP 探测（Windows 走 `SendARP`，不需要 raw socket），
  被占即跳过并记录事件
- 检测到同段其他 DHCP 服务器即在 UI 与日志告警（安全红线第 2 条；
  也是与 MAAS 共存的那道闸）

**验收**：实验网段内放一台已占静态 IP 的机器，该地址不被 OFFER 且事件可见；
在 MAAS 网段旁挂时 10 秒内出现"检测到其他 DHCP"告警。

## M8 · 现场交付强化（现场）

- 防火墙静默拦截可被检测（"监听中但 0 请求"给出排查指引）
- USB 网卡热插拔：枚举刷新、socket 重建、明确报错
- 空闲自动退出选项；`--open` + 前台 Ctrl-C 语义完整

**验收**：干净 Win11 + 非管理员账户 + 默认防火墙 + USB 网卡，
按 runbook 从零走通全流程；拔网卡可恢复。

## M9 · 真机 BMC 验证（现场形态发布 gate）

- ≥3 款真机 BMC（含 B300）：插线 → 租约 → discovery 确认闭环
- 每个新怪癖固化为回归测试或文档（option 61 已是先例）
- BMC 网页跳转是附带能力，**不设验收**

**前置风险要先认**：真机 BMC 行为全部未验证（现有矩阵是 VMware 固件 +
Linux 客户端）。若 B300 出厂 DHCP 关闭，"插线即得"第一步不成立 ——
兜底是 discovery（RMCP + 邻居表），本来就为不来要地址的机器设计。

## v1.0+ · 企业可靠性（常驻，以真实部署需求触发）

- **HA：多实例共享 PostgreSQL**（首选；自研同步协议只做兜底）——
  实例无状态化，冲突由数据库唯一约束仲裁；预估 1–2k 行（PG 后端 + 失效策略）
- 审计日志、权限分级
- 存储阶梯就此闭合：ephemeral → sqlite → 共享 PG

## v1.x · 协议扩展

- DHCPv6
- DDNS 联动（完整 DNS 服务器仍是非目标）

## 小项 backlog（不立里程碑）

- WebSocket 事件带 `vendor_class`（实时日志里认出设备类型）
- 租约行放 `http://<租约IP>` 链接（BMC 尾环的附带能力形态）
- 代码签名证书（v0.1 前决策点，客户现场 EDR/SmartScreen 门槛）
- cargo audit / deny 进 CI、发布物 SBOM
