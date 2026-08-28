<script>
  // 空状态下的开工表单：服务已经起来了，只是还没有作用域。
  // 选一块网卡就能推出子网，再给一段地址池即可开始发地址。
  import { onMount } from 'svelte'
  import { createScope, getInterfaces } from './api.js'

  let { oncreated } = $props()

  let ifaces = $state([])
  let picked = $state('')
  let poolStart = $state('')
  let poolEnd = $state('')
  let busy = $state(false)
  let err = $state(null)

  const current = $derived(ifaces.find((i) => i.name === picked))

  onMount(async () => {
    try {
      const list = await getInterfaces()
      ifaces = list.filter((i) => !i.isLoopback && i.ipv4.length > 0)
      picked = ifaces[0]?.name ?? ''
      suggest()
    } catch (e) {
      err = e.message
    }
  })

  // 给一段能直接用的默认池。/24 及更小的网段取 .100-.199；
  // 更大的网段猜不准，留空让人自己填。
  function suggest() {
    const c = current?.ipv4[0]
    if (!c) return
    if (c.prefix >= 24) {
      const base = c.addr.split('.').slice(0, 3).join('.')
      poolStart = `${base}.100`
      poolEnd = `${base}.199`
    } else {
      poolStart = ''
      poolEnd = ''
    }
  }

  async function submit(e) {
    e.preventDefault()
    const c = current?.ipv4[0]
    if (!c) return
    busy = true
    err = null
    try {
      await createScope({
        name: current.name,
        serverIp: c.addr,
        prefix: c.prefix,
        poolStart,
        poolEnd,
        router: c.addr,
      })
      oncreated?.()
    } catch (e2) {
      err = e2.message
    } finally {
      busy = false
    }
  }
</script>

<div class="wrap">
  <form onsubmit={submit}>
    <h2>开始发地址</h2>
    <p class="sub">服务已经在跑了，选一块网卡就能开工。</p>

    {#if ifaces.length === 0}
      <p class="err">没有找到可用的网卡（需要已配置 IPv4 地址的非环回网卡）。</p>
    {:else}
      <label for="iface">网卡</label>
      <select id="iface" bind:value={picked} onchange={suggest}>
        {#each ifaces as i (i.name)}
          <option value={i.name}>{i.name} — {i.ipv4[0].addr}/{i.ipv4[0].prefix}</option>
        {/each}
      </select>

      <label for="from">地址池</label>
      <div class="range">
        <input id="from" bind:value={poolStart} spellcheck="false" placeholder="起始" />
        <span>→</span>
        <input bind:value={poolEnd} spellcheck="false" placeholder="结束" aria-label="地址池结束" />
      </div>

      {#if err}<p class="err">{err}</p>{/if}

      <button type="submit" disabled={busy || !poolStart || !poolEnd}>
        {busy ? '创建中…' : '开始'}
      </button>
      <p class="hint">
        网段由所选网卡的地址推出。之后可以在这里继续加作用域、配静态保留和网络引导。
      </p>
    {/if}
  </form>
</div>

<style>
  .wrap { display: grid; place-items: center; padding: 48px 16px; }
  form {
    width: min(440px, 100%);
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 28px 30px;
  }
  h2 { margin: 0 0 4px; font-size: 18px; }
  .sub { margin: 0 0 22px; color: var(--muted); font-size: 14px; }
  label {
    display: block;
    font-size: 12px;
    color: var(--muted);
    margin-bottom: 5px;
  }
  select, input {
    width: 100%;
    padding: 8px 10px;
    font: inherit;
    font-size: 14px;
    color: var(--ink);
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 5px;
  }
  select { margin-bottom: 16px; }
  .range { display: flex; align-items: center; gap: 8px; margin-bottom: 18px; }
  .range span { color: var(--muted); }
  button {
    width: 100%;
    padding: 9px;
    font: inherit;
    font-size: 14px;
    color: #fff;
    background: var(--accent);
    border: 1px solid var(--accent);
    border-radius: 5px;
    cursor: pointer;
  }
  button:disabled { opacity: .55; cursor: default; }
  .hint { margin: 14px 0 0; font-size: 12.5px; color: var(--muted); }
  .err { margin: 0 0 14px; font-size: 13px; color: #b3452f; }
</style>
