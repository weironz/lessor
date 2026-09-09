<script>
  // 空状态下的开工表单：服务已经起来了，只是还没有作用域。
  // 直连作用域由本机网卡推导；经中继的远端网段则显式填写。
  import { onMount } from 'svelte'
  import { createScope, getInterfaces } from './api.js'

  let { oncreated, oncancel = null, compact = false } = $props()

  let ifaces = $state([])
  let mode = $state('direct')
  let picked = $state('')
  let relayListener = $state('')
  let relayName = $state('')
  let relaySubnet = $state('')
  let relayPrefix = $state('24')
  let relayRouter = $state('')
  let poolStart = $state('')
  let poolEnd = $state('')
  let busy = $state(false)
  let err = $state(null)

  // 一张网卡可能同时有普通地址和 APIPA 地址。地址而非网卡名才是
  // DHCP 的监听点，因此必须让用户明确选择哪一个。
  const addresses = $derived(
    ifaces.flatMap((i) => i.ipv4.map((cidr) => ({
      key: `${i.name}\u0000${cidr.addr}/${cidr.prefix}`,
      name: i.name,
      cidr,
    }))),
  )
  const current = $derived(addresses.find((i) => i.key === picked))
  const currentRelayListener = $derived(addresses.find((i) => i.key === relayListener))

  onMount(async () => {
    try {
      const list = await getInterfaces()
      ifaces = list.filter((i) => !i.isLoopback && i.ipv4.length > 0)
      // 不自动选第一块网卡：虚拟网卡、VPN 往往排在物理网卡之前，
      // 误选后看似服务已启动，实际却永远收不到直连设备的广播。
      picked = ''
      relayListener = ''
    } catch (e) {
      err = e.message
    }
  })

  // 给一段能直接用的默认池。/24 及更小的网段取 .100-.199；
  // 更大的网段猜不准，留空让人自己填。
  function suggest() {
    const c = current?.cidr
    if (!c) return
    if (c.prefix >= 24) {
      const base = c.addr.split('.').slice(0, 3).join('.')
      const host = Number(c.addr.split('.')[3])
      // 默认池绝不能包含服务端自己的监听地址。
      const range = [[2, 99], [101, 199], [200, 254]]
        .find(([start, end]) => host < start || host > end)
      poolStart = `${base}.${range[0]}`
      poolEnd = `${base}.${range[1]}`
    } else {
      poolStart = ''
      poolEnd = ''
    }
  }

  function suggestRelay() {
    if (Number(relayPrefix) !== 24 || !/^\d{1,3}(?:\.\d{1,3}){3}$/.test(relaySubnet)) return
    const base = relaySubnet.split('.').slice(0, 3).join('.')
    poolStart = `${base}.100`
    poolEnd = `${base}.199`
  }

  async function submit(e) {
    e.preventDefault()
    const c = current?.cidr
    const listener = currentRelayListener?.cidr
    if (mode === 'direct' && !c) return
    if (mode === 'relay' && !listener) return
    busy = true
    err = null
    try {
      const body = mode === 'relay'
        ? {
            name: relayName || `${relaySubnet}/${relayPrefix}（经中继）`,
            serverIp: listener.addr,
            subnet: relaySubnet,
            prefix: Number(relayPrefix),
            poolStart,
            poolEnd,
            router: relayRouter,
            viaRelay: true,
          }
        : {
            name: current.name,
            serverIp: c.addr,
            prefix: c.prefix,
            poolStart,
            poolEnd,
            router: c.addr,
          }
      const created = await createScope(body)
      oncreated?.({
        scopeId: created.id,
        serverIp: mode === 'relay' ? listener.addr : c.addr,
        prefix: mode === 'relay' ? Number(relayPrefix) : c.prefix,
        viaRelay: mode === 'relay',
      })
    } catch (e2) {
      err = e2.message
    } finally {
      busy = false
    }
  }
</script>

<div class:compact class="wrap">
  <form onsubmit={submit}>
    <h2>{compact ? '新增作用域' : '开始发地址'}</h2>
    <p class="sub">
      {compact
        ? '可新增直连网段，或添加一个由交换机 DHCP Relay 转发的远端网段。'
        : '服务已经在跑了，选择直连网卡或配置一个经 Relay 的远端网段。'}
    </p>

    {#if ifaces.length === 0}
      <p class="err">没有找到可用的网卡（需要已配置 IPv4 地址的非环回网卡）。</p>
    {:else}
      <fieldset class="mode">
        <legend>服务方式</legend>
        <label><input type="radio" name="mode" value="direct" bind:group={mode} /> 直连 DHCP</label>
        <label><input type="radio" name="mode" value="relay" bind:group={mode} /> 经 DHCP Relay</label>
      </fieldset>

      {#if mode === 'direct'}
        <label for="iface">网卡</label>
        <select id="iface" bind:value={picked} onchange={suggest}>
          <option value="" disabled>请选择实际连接设备的网卡</option>
          {#each addresses as item (item.key)}
            <option value={item.key}>{item.name} — {item.cidr.addr}/{item.cidr.prefix}</option>
          {/each}
        </select>
      {:else}
        <p class="hint relay-hint">远端 VLAN 不会出现在本机网卡列表。Relay 会将请求单播到下面选定的 lessor 监听地址。</p>
        <label for="relay-listener">lessor 监听地址</label>
        <select id="relay-listener" bind:value={relayListener}>
          <option value="" disabled>请选择 lessor 本机地址</option>
          {#each addresses as item (item.key)}
            <option value={item.key}>{item.name} — {item.cidr.addr}/{item.cidr.prefix}</option>
          {/each}
        </select>
        <label for="relay-name">作用域名称</label>
        <input id="relay-name" bind:value={relayName} placeholder="例如 BMC Vlan40" />
        <div class="split">
          <div>
            <label for="relay-subnet">远端网段</label>
            <input id="relay-subnet" bind:value={relaySubnet} onchange={suggestRelay} placeholder="172.16.40.0" spellcheck="false" />
          </div>
          <div>
            <label for="relay-prefix">前缀</label>
            <input id="relay-prefix" bind:value={relayPrefix} onchange={suggestRelay} inputmode="numeric" spellcheck="false" />
          </div>
        </div>
        <label for="relay-router">网关 / Relay giaddr</label>
        <input id="relay-router" bind:value={relayRouter} placeholder="172.16.40.254" spellcheck="false" />
      {/if}

      <label for="from">地址池</label>
      <div class="range">
        <input id="from" bind:value={poolStart} spellcheck="false" placeholder="起始" />
        <span>→</span>
        <input bind:value={poolEnd} spellcheck="false" placeholder="结束" aria-label="地址池结束" />
      </div>

      {#if err}<p class="err">{err}</p>{/if}

      <button type="submit" disabled={busy || !poolStart || !poolEnd || (mode === 'direct' ? !picked : !relayListener || !relaySubnet || !relayPrefix || !relayRouter)}>
        {busy ? '创建中…' : '开始'}
      </button>
      {#if oncancel}
        <button type="button" class="cancel" onclick={oncancel} disabled={busy}>取消</button>
      {/if}
      <p class="hint">
        {mode === 'direct'
          ? '网段由所选网卡的地址推出。之后可以在这里继续加作用域、配静态保留和网络引导。'
          : 'Relay 地址应是客户端 VLAN 网关的 giaddr，例如 Vlan40 的 172.16.40.254。'}
      </p>
    {/if}
  </form>
</div>

<style>
  .wrap { display: grid; place-items: center; padding: 48px 16px; }
  .wrap.compact { place-items: stretch; padding: 0; margin-bottom: 18px; }
  .compact form { width: auto; padding: 20px; }
  form { width: min(440px, 100%); background: var(--panel); border: 1px solid var(--line); border-radius: 8px; padding: 28px 30px; }
  h2 { margin: 0 0 4px; font-size: 18px; }
  .sub { margin: 0 0 22px; color: var(--muted); font-size: 14px; }
  label { display: block; font-size: 12px; color: var(--muted); margin-bottom: 5px; }
  select, input { width: 100%; padding: 8px 10px; font: inherit; font-size: 14px; color: var(--ink); background: var(--bg); border: 1px solid var(--line); border-radius: 5px; }
  select { margin-bottom: 16px; }
  .mode { display: flex; gap: 18px; margin: 0 0 16px; padding: 0; border: 0; }
  .mode legend { margin-bottom: 7px; font-size: 12px; color: var(--muted); }
  .mode label { display: flex; align-items: center; gap: 6px; margin: 0; color: var(--ink); }
  .mode input { width: auto; }
  .split { display: grid; grid-template-columns: 1fr 100px; gap: 10px; }
  .split input { margin-bottom: 16px; }
  .range { display: flex; align-items: center; gap: 8px; margin-bottom: 18px; }
  .range span { color: var(--muted); }
  button { width: 100%; padding: 9px; font: inherit; font-size: 14px; color: #fff; background: var(--accent); border: 1px solid var(--accent); border-radius: 5px; cursor: pointer; }
  button:disabled { opacity: .55; cursor: default; }
  button.cancel { margin-top: 8px; color: var(--ink); background: transparent; border-color: var(--line); }
  .hint { margin: 14px 0 0; font-size: 12.5px; color: var(--muted); }
  .relay-hint { margin: -2px 0 14px; }
  .err { margin: 0 0 14px; font-size: 13px; color: #b3452f; }
</style>
