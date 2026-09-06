<script>
  import { onMount } from 'svelte'
  import { discover, getInterfaces, getState } from './api.js'

  let ifaces = $state([])
  let picked = $state('')
  let sweep = $state(true)
  let running = $state(false)
  let found = $state(null)
  let err = $state(null)

  // 只列出能在上面做发现的网卡：非环回、有 IPv4 地址
  const usable = $derived(ifaces.filter((i) => !i.isLoopback && i.ipv4.length > 0))
  // 不能只取每张网卡的第一个 IPv4：Windows 会把 APIPA 排在手工地址前。
  const addresses = $derived(
    usable.flatMap((i) => i.ipv4.map((cidr) => ({
      key: `${i.name}\u0000${cidr.addr}/${cidr.prefix}`,
      name: i.name,
      cidr,
    }))),
  )
  const current = $derived(addresses.find((i) => i.key === picked))

  onMount(async () => {
    try {
      const [list, state] = await Promise.all([getInterfaces(), getState()])
      ifaces = list
      const available = list
        .filter((i) => !i.isLoopback && i.ipv4.length > 0)
        .flatMap((i) => i.ipv4.map((cidr) => ({
          key: `${i.name}\u0000${cidr.addr}/${cidr.prefix}`,
          cidr,
        })))
      // 发现页优先选 DHCP 正在监听的地址；没有作用域时才退回第一个地址。
      const listenerIp = state.listeners?.find((l) =>
        available.some((item) => item.cidr.addr === l.serverIp),
      )?.serverIp
      picked = available.find((item) => item.cidr.addr === listenerIp)?.key ?? available[0]?.key ?? ''
    } catch (e) {
      err = e.message
    }
  })

  async function run() {
    const cidr = current?.cidr
    if (!cidr) return
    running = true
    err = null
    try {
      found = await discover(cidr.addr, cidr.prefix, sweep)
    } catch (e) {
      err = e.message
    } finally {
      running = false
    }
  }

  const VIA = { rmcp: 'IPMI 应答', probed: '探测发现', neighbor: '邻居表' }
</script>

<p class="intro">
  DHCP 只能看见来要地址的机器。已经配了静态 IP 的设备不会发请求 ——
  这里用 IPMI 的 RMCP 探测、UDP 探测加邻居表把它们找出来。
</p>

<div class="controls">
  <label>
    网卡
    <select bind:value={picked} disabled={running || usable.length === 0}>
      {#each addresses as item (item.key)}
        <option value={item.key}>{item.name} — {item.cidr.addr}/{item.cidr.prefix}</option>
      {/each}
    </select>
  </label>

  <label class="check">
    <input type="checkbox" bind:checked={sweep} disabled={running} />
    逐个探测整个网段
  </label>

  <button class="primary" onclick={run} disabled={running || !current}>
    {running ? '扫描中…' : '开始扫描'}
  </button>
</div>

{#if usable.length === 0 && !err}
  <div class="empty">没有可用于扫描的网卡。</div>
{/if}

{#if err}<p class="err">{err}</p>{/if}

{#if found}
  {#if found.length === 0}
    <div class="empty">这个网段上没有发现设备。</div>
  {:else}
    <div class="tablewrap">
      <table>
        <thead>
          <tr>
            <th>地址</th>
            <th>MAC</th>
            <th>类型</th>
            <th>发现方式</th>
            <th>说明</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {#each found as d (d.ip)}
            <tr>
              <td class="mono">{d.ip}</td>
              <td class="mono">{d.mac ?? '—'}</td>
              <td>
                {#if d.isBmc}
                  <span class="pill accent">BMC</span>
                {:else}
                  <span class="muted">设备</span>
                {/if}
              </td>
              <td class="muted">{d.via.map((v) => VIA[v] ?? v).join('、')}</td>
              <td class="muted">{d.note ?? ''}</td>
              <td>
                <a href={`https://${d.ip}/`} target="_blank" rel="noreferrer">打开</a>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
{/if}

<style>
  .intro {
    margin: 0 0 16px;
    max-width: 68ch;
    color: var(--ink-2);
    font-size: 13px;
  }
  .controls {
    display: flex;
    align-items: center;
    gap: 16px;
    flex-wrap: wrap;
    margin-bottom: 16px;
    font-size: 13px;
  }
  label {
    display: flex;
    align-items: center;
    gap: 7px;
    color: var(--muted);
  }
  .check { cursor: pointer; }
  .err {
    margin: 0 0 12px;
    padding: 8px 12px;
    background: var(--danger-bg);
    color: var(--danger);
    border-radius: var(--radius);
    font-size: 13px;
  }
  a { color: var(--accent); }
</style>
