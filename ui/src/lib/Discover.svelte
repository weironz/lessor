<script>
  import { onDestroy, onMount } from 'svelte'
  import { discoverScope, getDevices, getState } from './api.js'

  // target 只在“刚新建作用域”的那一次携带 scanId。正常进入页面只读取缓存。
  let { target = null, onautostart = null } = $props()

  let scopes = $state([])
  let collections = $state([])
  let selectedScope = $state('')
  let runningScope = $state(null)
  let autoMessage = $state(null)
  let err = $state(null)
  let activeRun = 0
  let consumedScanId = ''

  const directScopes = $derived(
    scopes.filter((scope) => scope.enabled && !scope.viaRelay && scope.listenerPresent),
  )
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
      const [state, cached] = await Promise.all([getState(), getDevices()])
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
    // 页面留着时只刷新已保存的结果；不会额外制造一轮网络扫描。
    const timer = setInterval(() => void loadCached(), 15_000)
    return () => clearInterval(timer)
  })

  function replaceCollection(collection) {
    collections = [...collections.filter((item) => item.scope.id !== collection.scope.id), collection]
      .sort((left, right) => left.scope.id - right.scope.id)
  }

  async function scan(scopeId, { quiet = false } = {}) {
    runningScope = scopeId
    if (!quiet) {
      err = null
      autoMessage = null
    }
    try {
      const result = await discoverScope(scopeId, { sweep: true, waitMs: 1000 })
      replaceCollection(result.data)
      return result.data
    } catch (e) {
      if (!quiet) err = e.message
      throw e
    } finally {
      if (runningScope === scopeId) runningScope = null
    }
  }

  async function runAutomatic(scope) {
    const run = ++activeRun
    const attempts = 10
    for (let attempt = 1; attempt <= attempts; attempt += 1) {
      if (run !== activeRun) return
      autoMessage = `正在等待 BMC 获取 DHCP 地址并确认 IPMI；第 ${attempt}/${attempts} 次扫描…`
      try {
        const collection = await scan(scope.scopeId, { quiet: true })
        if (run !== activeRun) return
        if (collection.devices.some((device) => device.kind === 'bmc')) {
          autoMessage = '已确认 BMC；该地址已保存在此作用域的发现结果中。'
          return
        }
      } catch (e) {
        if (run === activeRun) {
          err = e.message
          autoMessage = null
        }
        return
      }
      if (attempt < attempts) await new Promise((resolve) => setTimeout(resolve, 1500))
    }
    if (run === activeRun) {
      autoMessage = '暂未确认 BMC。请确认网线、BMC 已开机且仍为 DHCP 模式，然后点“刷新发现”。'
    }
  }

  $effect(() => {
    const scanId = target?.scanId
    if (!scanId || scanId === consumedScanId) return
    consumedScanId = scanId
    onautostart?.()
    void runAutomatic(target)
  })

  function refreshSelected() {
    if (!selectedScope) return
    void scan(Number(selectedScope)).catch(() => {})
  }

  function fmtTime(unix) {
    if (!unix) return '—'
    return new Date(unix * 1000).toLocaleString('zh-CN', { hour12: false })
  }

  const VIA = { rmcp: 'IPMI 应答', probed: '探测发现', neighbor: '邻居表' }

  onDestroy(() => {
    activeRun += 1
  })
</script>

<p class="intro">
  发现结果按 DHCP 作用域保存。进入本页只读取缓存；只有新建作用域或对某个作用域点“刷新发现”才会执行逐地址探测。
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
  <button class="primary" onclick={refreshSelected} disabled={!selectedScope || runningScope !== null}>
    {runningScope !== null ? '扫描中…' : '刷新发现'}
  </button>
  <button class="secondary" onclick={() => void loadCached()} disabled={runningScope !== null}>刷新列表</button>
</div>

{#if target && autoMessage}
  <div class="auto">
    <strong>新作用域 {target.serverIp}/{target.prefix}</strong>
    <span>{autoMessage}</span>
  </div>
{/if}

{#if unscannedScopes.length}
  <p class="hint">
    尚未扫描：{unscannedScopes.map((scope) => `${scope.name}（${scope.subnet}/${scope.prefix}）`).join('、')}。
    选择一个作用域后点“刷新发现”。
  </p>
{/if}

{#if err}<p class="err">{err}</p>{/if}

{#if rows.length === 0}
  <div class="empty">{collections.length ? '所选作用域暂未发现设备。' : '还没有已保存的发现结果。'}</div>
{:else}
  <div class="tablewrap">
    <table>
      <thead>
        <tr>
          <th>作用域</th>
          <th>网段 / 监听地址</th>
          <th>地址</th>
          <th>MAC</th>
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
  select { min-width: 260px; padding: 7px 9px; font: inherit; color: var(--ink); background: var(--bg); border: 1px solid var(--line); border-radius: 5px; }
  .primary, .secondary { padding: 8px 10px; font: inherit; font-size: 13px; border-radius: 5px; cursor: pointer; }
  .primary { color: #fff; background: var(--accent); border: 1px solid var(--accent); }
  .secondary { color: var(--ink); background: transparent; border: 1px solid var(--line); }
  button:disabled { opacity: .55; cursor: default; }
  .auto, .hint { margin: 0 0 12px; padding: 9px 12px; background: var(--panel); border: 1px solid var(--line); border-radius: var(--radius); font-size: 13px; }
  .auto { display: flex; gap: 10px; flex-wrap: wrap; }
  .hint { color: var(--muted); }
  .err { margin: 0 0 12px; padding: 8px 12px; background: var(--danger-bg); color: var(--danger); border-radius: var(--radius); font-size: 13px; }
  td .muted { display: block; margin-top: 2px; font-size: 11px; }
  a { color: var(--accent); }
</style>
