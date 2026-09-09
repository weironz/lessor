<script>
  import { onMount } from 'svelte'
  import { discoverScope, getDevices, getState } from './api.js'

  let { target = null, settings = null } = $props()

  let scopes = $state([])
  let collections = $state([])
  let selectedScope = $state('')
  let typeFilter = $state('')
  let runningScope = $state(null)
  let err = $state(null)

  const directScopes = $derived(
    scopes.filter((scope) => scope.enabled && !scope.viaRelay && scope.listenerPresent),
  )
  const discoveryDisabled = $derived(settings?.mode === 'disabled')
  const visibleCollections = $derived(
    selectedScope
      ? collections.filter((collection) => String(collection.scope.id) === selectedScope)
      : collections,
  )
  const rows = $derived(
    visibleCollections.flatMap((collection) =>
      collection.devices.map((device) => ({ ...device, scope: collection.scope })),
    ),
  )
  const cachedScopeIds = $derived(new Set(collections.map((collection) => String(collection.scope.id))))
  const unscannedScopes = $derived(directScopes.filter((scope) => !cachedScopeIds.has(String(scope.id))))

  async function loadCached() {
    try {
      const [state, cached] = await Promise.all([
        getState(),
        getDevices({ kind: typeFilter || undefined }),
      ])
      scopes = state.scopes
      collections = cached.data ?? []
      if (target?.scopeId && !selectedScope) selectedScope = String(target.scopeId)
      err = null
    } catch (e) {
      err = e.message
    }
  }

  onMount(() => {
    void loadCached()
    // 页面留着时只刷新缓存；不会额外制造一轮网络扫描。
    const timer = setInterval(() => void loadCached(), 15_000)
    return () => clearInterval(timer)
  })

  async function scan(scopeId) {
    if (discoveryDisabled) return
    runningScope = scopeId
    err = null
    try {
      // 全范围探测只发生在用户明确点击这个按钮时。
      await discoverScope(scopeId, { sweep: true, waitMs: 1000 })
      await loadCached()
    } catch (e) {
      err = e.message
    } finally {
      if (runningScope === scopeId) runningScope = null
    }
  }

  function refreshSelected() {
    if (!selectedScope) return
    void scan(Number(selectedScope))
  }

  function fmtTime(unix) {
    if (!unix) return '—'
    return new Date(unix * 1000).toLocaleString('zh-CN', { hour12: false })
  }

  const VIA = { rmcp: 'IPMI 应答', probed: '探测发现', neighbor: '邻居表' }
</script>

<p class="intro">
  发现结果按 DHCP 作用域保存。进入本页只读取缓存；逐地址探测只能由你选择作用域后手动触发。
</p>

<div class="controls">
  <label>
    作用域
    <select bind:value={selectedScope} disabled={runningScope !== null}>
      <option value="">全部已扫描作用域</option>
      {#each directScopes as scope (scope.id)}
        <option value={String(scope.id)}>{scope.name} — {scope.subnet}/{scope.prefix}</option>
      {/each}
    </select>
  </label>
  <label>
    类型
    <select bind:value={typeFilter} onchange={() => void loadCached()} disabled={runningScope !== null}>
      <option value="">全部类型</option>
      <option value="bmc">仅 BMC（IPMI/RMCP 已确认）</option>
      <option value="device">其他设备</option>
    </select>
  </label>
  <button class="primary" onclick={refreshSelected} disabled={!selectedScope || runningScope !== null || discoveryDisabled}>
    {runningScope !== null ? '扫描中…' : '刷新发现'}
  </button>
  <button class="secondary" onclick={() => void loadCached()} disabled={runningScope !== null}>刷新列表</button>
</div>

{#if discoveryDisabled}
  <p class="hint">设备发现已禁用；当前仅展示已保存的缓存。可在“设置”页重新启用。</p>
{/if}

{#if unscannedScopes.length}
  <p class="hint">
    尚未扫描：{unscannedScopes.map((scope) => `${scope.name}（${scope.subnet}/${scope.prefix}）`).join('、')}。
    选择一个作用域后点“刷新发现”。
  </p>
{/if}

{#if err}<p class="err">{err}</p>{/if}

{#if rows.length === 0}
  <div class="empty">{collections.length ? '所选作用域暂未发现此类型设备。' : '还没有已保存的发现结果。'}</div>
{:else}
  <div class="tablewrap">
    <table>
      <thead>
        <tr>
          <th>作用域</th>
          <th>网段 / 监听地址</th>
          <th>地址</th>
          <th>MAC</th>
          <th>DHCP 状态</th>
          <th>类型</th>
          <th>发现方式</th>
          <th>最近发现</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {#each rows as device (`${device.scope.id}:${device.ip}`)}
          <tr>
            <td>{device.scope.name}<span class="muted mono">#{device.scope.id}</span></td>
            <td class="mono">{device.scope.subnet}/{device.scope.prefix}<br /><span class="muted">{device.scope.serverIp ?? '经中继'}</span></td>
            <td class="mono">{device.ip}</td>
            <td class="mono">{device.mac ?? '—'}</td>
            <td>
              {#if device.activeLease}
                <span class="pill current">当前租约</span>
              {:else if device.kind === 'bmc'}
                <span class="pill stale">历史发现</span>
              {:else}
                <span class="muted">未租约</span>
              {/if}
            </td>
            <td>
              {#if device.kind === 'bmc'}
                <span class="pill accent">BMC</span>
              {:else}
                <span class="muted">设备</span>
              {/if}
            </td>
            <td class="muted">{device.sources.map((source) => VIA[source] ?? source).join('、')}</td>
            <td class="muted" title={`首次：${fmtTime(device.firstSeen)}`}>{fmtTime(device.lastSeen)}</td>
            <td><a href={`https://${device.ip}/`} target="_blank" rel="noreferrer">打开</a></td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}

<style>
  .intro { margin: 0 0 16px; max-width: 74ch; color: var(--ink-2); font-size: 13px; }
  .controls { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; margin-bottom: 14px; font-size: 13px; }
  label { display: flex; align-items: center; gap: 7px; color: var(--muted); }
  select { min-width: 190px; padding: 7px 9px; font: inherit; color: var(--ink); background: var(--bg); border: 1px solid var(--line); border-radius: 5px; }
  .primary, .secondary { padding: 8px 10px; font: inherit; font-size: 13px; border-radius: 5px; cursor: pointer; }
  .primary { color: #fff; background: var(--accent); border: 1px solid var(--accent); }
  .secondary { color: var(--ink); background: transparent; border: 1px solid var(--line); }
  button:disabled { opacity: .55; cursor: default; }
  .hint { margin: 0 0 12px; padding: 9px 12px; color: var(--muted); background: var(--panel); border: 1px solid var(--line); border-radius: var(--radius); font-size: 13px; }
  .err { margin: 0 0 12px; padding: 8px 12px; background: var(--danger-bg); color: var(--danger); border-radius: var(--radius); font-size: 13px; }
  td .muted { display: block; margin-top: 2px; font-size: 11px; }
  a { color: var(--accent); }
  .pill.current { color: #17653e; border-color: #8fc9a6; background: #e6f5eb; }
  .pill.stale { color: #74623c; border-color: #d8c889; background: #faf4dd; }
</style>
