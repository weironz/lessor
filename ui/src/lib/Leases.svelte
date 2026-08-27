<script>
  import { revokeLease, fmtDuration } from './api.js'

  let { leases = [], onchange } = $props()
  let busy = $state(null)
  let err = $state(null)

  const now = () => Math.floor(Date.now() / 1000)

  const STATE_STYLE = {
    bound: 'ok',
    offered: 'accent',
    declined: 'danger',
    free: '',
  }

  async function revoke(l) {
    busy = `${l.scopeId}/${l.ip}`
    err = null
    try {
      await revokeLease(l.scopeId, l.ip)
      onchange?.()
    } catch (e) {
      err = e.message
    } finally {
      busy = null
    }
  }
</script>

{#if err}<p class="err">{err}</p>{/if}

{#if leases.length === 0}
  <div class="empty">还没有租约。设备接上并发出 DHCP 请求后会出现在这里。</div>
{:else}
  <div class="tablewrap">
    <table>
      <thead>
        <tr>
          <th>地址</th>
          <th>客户端</th>
          <th>主机名</th>
          <th>状态</th>
          <th>剩余</th>
          <th>设备类型</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {#each leases as l (`${l.scopeId}/${l.ip}`)}
          <tr>
            <td class="mono">{l.ip}</td>
            <td class="mono">{l.client.value}</td>
            <td>{l.hostname ?? '—'}</td>
            <td><span class="pill {STATE_STYLE[l.state] ?? ''}">{l.state}</span></td>
            <td class="mono">{fmtDuration(l.expiresAt - now())}</td>
            <td class="muted vendor" title={l.vendorClass ?? ''}>
              {l.vendorClass ?? '—'}
            </td>
            <td>
              <button
                class="danger"
                disabled={busy === `${l.scopeId}/${l.ip}`}
                onclick={() => revoke(l)}
              >撤销</button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}

<style>
  .err {
    margin: 0 0 12px;
    padding: 8px 12px;
    background: var(--danger-bg);
    color: var(--danger);
    border-radius: var(--radius);
    font-size: 13px;
  }
  .vendor {
    max-width: 260px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
