<script>
  import { onMount } from 'svelte'
  import { connectEvents, getLeases, getState, fmtDuration } from './lib/api.js'
  import Scopes from './lib/Scopes.svelte'
  import Leases from './lib/Leases.svelte'
  import Events from './lib/Events.svelte'
  import Discover from './lib/Discover.svelte'
  import NewScope from './lib/NewScope.svelte'

  let tab = $state('scopes')
  let status = $state('connecting')
  let state = $state(null)
  let leases = $state([])
  let packets = $state([])
  let error = $state(null)

  const TABS = [
    { id: 'scopes', label: '作用域' },
    { id: 'leases', label: '租约' },
    { id: 'events', label: '实时日志' },
    { id: 'discover', label: '发现设备' },
  ]

  async function refresh() {
    try {
      ;[state, leases] = await Promise.all([getState(), getLeases()])
      error = null
    } catch (e) {
      error = e.message
    }
  }

  onMount(() => {
    refresh()
    // 状态里的容量和运行时长不会通过事件推送，定时兜一下
    const tick = setInterval(refresh, 10_000)
    const stop = connectEvents({
      onStatus: (s) => (status = s),
      onEvent: (ev) => {
        if (ev.kind === 'packet') {
          // 只留最近 300 条，界面不该无限增长
          packets = [ev, ...packets].slice(0, 300)
        } else if (
          ev.kind === 'leasesChanged' ||
          ev.kind === 'reaped' ||
          ev.kind === 'scopesChanged'
        ) {
          refresh()
        }
      },
    })
    return () => {
      clearInterval(tick)
      stop()
    }
  })

  // 服务起来了但一个作用域都没有 —— 这不是错误状态，是"还没开工"。
  // 此时整个界面就是一张开工表单，不摆一堆空标签页。
  const needsSetup = $derived(state !== null && state.scopes.length === 0)

  const statusLabel = $derived(
    { online: '已连接', offline: '连接中断', connecting: '连接中' }[status] ?? status,
  )
</script>

<header>
  <div class="brand">
    <h1>lessor</h1>
    {#if state}<span class="ver mono">v{state.version}</span>{/if}
  </div>

  <div class="meta">
    {#if state}
      <span class="muted">已运行 {fmtDuration(state.uptimeSecs)}</span>
    {/if}
    <span class="pill {status === 'online' ? 'ok' : 'danger'}">{statusLabel}</span>
  </div>
</header>

{#if error}
  <p class="err">接口出错：{error}</p>
{/if}

<!--
  已经在监听、却一个请求都没收到。这不是错误（网段上可能真的还没人要地址），
  所以用 warn 而不是 err；但现场十有八九是防火墙，值得主动摆出来 ——
  装机的人盯着的是这个界面，不是终端里的日志。
-->
{#if state?.quietNote}
  <details class="warn" open>
    <summary>{state.quietNote}</summary>
    <pre>{state.quietHint}</pre>
  </details>
{/if}

{#if needsSetup}
  <NewScope oncreated={refresh} />
{:else}
<nav>
  {#each TABS as t (t.id)}
    <button class:active={tab === t.id} onclick={() => (tab = t.id)}>
      {t.label}
      {#if t.id === 'leases' && leases.length}
        <span class="count">{leases.length}</span>
      {/if}
    </button>
  {/each}
</nav>

<main>
  {#if tab === 'scopes'}
    <Scopes scopes={state?.scopes ?? []} listeners={state?.listeners ?? []} onchange={refresh} />
  {:else if tab === 'leases'}
    <Leases {leases} onchange={refresh} />
  {:else if tab === 'events'}
    <Events {packets} onclear={() => (packets = [])} />
  {:else}
    <Discover />
  {/if}
</main>
{/if}

<style>
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 14px 20px;
    border-bottom: 1px solid var(--line);
    background: var(--panel);
  }
  .brand {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  h1 {
    font-size: 18px;
    letter-spacing: -0.01em;
  }
  .ver {
    font-size: 12px;
    color: var(--muted);
  }
  .meta {
    display: flex;
    align-items: center;
    gap: 12px;
    font-size: 13px;
  }
  .err {
    margin: 0;
    padding: 8px 20px;
    background: var(--danger-bg);
    color: var(--danger);
    border-bottom: 1px solid var(--danger);
    font-size: 13px;
  }
  /* 提醒，不是错误 —— 网段上真的没人要地址时它也会出现 */
  .warn {
    padding: 8px 20px;
    background: var(--warn-bg);
    color: var(--warn);
    border-bottom: 1px solid var(--warn);
    font-size: 13px;
  }
  .warn summary {
    cursor: pointer;
  }
  .warn pre {
    margin: 8px 0 0;
    font-size: 12px;
    line-height: 1.6;
    white-space: pre-wrap;
    overflow-x: auto;
  }
  nav {
    display: flex;
    gap: 2px;
    padding: 0 16px;
    border-bottom: 1px solid var(--line);
    background: var(--panel);
  }
  nav button {
    border: 0;
    border-bottom: 2px solid transparent;
    border-radius: 0;
    background: transparent;
    color: var(--muted);
    padding: 9px 12px;
  }
  nav button:hover {
    color: var(--ink);
  }
  nav button.active {
    color: var(--accent);
    border-bottom-color: var(--accent);
  }
  .count {
    display: inline-block;
    margin-left: 5px;
    padding: 0 6px;
    border-radius: 20px;
    background: var(--panel-2);
    font-family: var(--mono);
    font-size: 11px;
  }
  main {
    padding: 20px;
    max-width: 1400px;
  }
</style>
