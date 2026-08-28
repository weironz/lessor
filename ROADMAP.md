# 路线图

里程碑制，不排日历。**这里是开发计划的唯一事实来源** ——
做完的条目移入 [ROADMAP-done.md](ROADMAP-done.md)（带完成日期与验收证据）。
每条决策的"为什么"在 [docs/design-and-plan.md](docs/design-and-plan.md)。

两种运行形态：**现场**（field，笔记本临时用）与**常驻**（resident，长期服务）。
标注在各条目上。

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
