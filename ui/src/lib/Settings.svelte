<script>
  import { patchSettings } from './api.js'

  let { settings = null, onchange = null } = $props()
  let mode = $state('manual')
  let intervalMins = $state(15)
  let busy = $state(false)
  let err = $state(null)
  let synced = ''

  // 后端每十秒刷新状态；只有设置值真的变化且当前没有提交时才覆盖表单。
  $effect(() => {
    const key = JSON.stringify(settings ?? {})
    if (!busy && key !== synced) {
      mode = settings?.mode ?? 'manual'
      intervalMins = Math.max(5, Math.round((settings?.intervalSecs ?? 900) / 60))
      synced = key
    }
  })

  async function save() {
    busy = true
    err = null
    try {
      await patchSettings({
        discoveryMode: mode,
        discoveryIntervalSecs: Number(intervalMins) * 60,
      })
      onchange?.()
    } catch (e) {
      err = e.message
    } finally {
      busy = false
    }
  }
</script>

<section class="card">
  <h2>设备发现</h2>
  <p class="sub">DHCP 发地址与设备发现相互独立。默认不主动扫描，避免在陌生网段产生扫描流量。</p>

  <label for="mode">自动发现</label>
  <select id="mode" bind:value={mode} disabled={busy}>
    <option value="manual">仅手动触发（默认）</option>
    <option value="periodic">按固定间隔轻量刷新</option>
    <option value="disabled">禁用设备发现</option>
  </select>

  {#if mode === 'periodic'}
    <label for="interval">刷新间隔（分钟）</label>
    <input id="interval" type="number" min="5" max="1440" step="1" bind:value={intervalMins} disabled={busy} />
    <p class="hint">周期刷新只发送 RMCP 广播并读取邻居表，不会逐个扫描地址；最短 5 分钟。</p>
  {:else if mode === 'manual'}
    <p class="hint">新建作用域后不会自动扫描。在“发现设备”选择作用域后，手动点“刷新发现”。</p>
  {:else}
    <p class="hint">已禁用时，界面和 API 都不会发起设备发现；现有缓存仍可查看。</p>
  {/if}

  {#if err}<p class="err">{err}</p>{/if}
  <button onclick={save} disabled={busy || (mode === 'periodic' && (intervalMins < 5 || intervalMins > 1440))}>
    {busy ? '保存中…' : '保存设置'}
  </button>
</section>

<section class="card safety">
  <h2>网卡自动建议</h2>
  <p>lessor 不会根据默认网关自动建立 DHCP 作用域。默认路由通常属于 Wi‑Fi 或生产网卡，在该网段静默启用 DHCP 可能影响已有网络。</p>
  <p>后续可根据路由和物理网卡状态给出“推荐网卡”，但仍须由你确认地址池后创建作用域。</p>
</section>

<style>
  .card { max-width: 620px; margin-bottom: 16px; padding: 18px 20px; background: var(--panel); border: 1px solid var(--line); border-radius: var(--radius); }
  h2 { margin: 0 0 6px; font-size: 16px; }
  .sub, .hint, .safety p { color: var(--muted); font-size: 13px; line-height: 1.55; }
  .sub { margin: 0 0 18px; }
  label { display: block; margin: 14px 0 5px; color: var(--muted); font-size: 12px; }
  select, input { box-sizing: border-box; width: min(320px, 100%); padding: 8px 10px; font: inherit; color: var(--ink); background: var(--bg); border: 1px solid var(--line); border-radius: 5px; }
  button { margin-top: 8px; padding: 8px 12px; font: inherit; color: #fff; background: var(--accent); border: 1px solid var(--accent); border-radius: 5px; cursor: pointer; }
  button:disabled { opacity: .55; cursor: default; }
  .err { color: var(--danger); font-size: 13px; }
  .safety { border-left: 3px solid #b47a17; }
</style>
