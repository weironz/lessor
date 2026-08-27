<script>
  import { fmtClock } from './api.js'

  let { packets = [], onclear } = $props()

  const RESULT_STYLE = {
    OFFER: 'accent',
    ACK: 'ok',
    NAK: 'warn',
    DROP: 'danger',
    HANDLED: '',
  }
</script>

<div class="bar">
  <span class="muted">
    实时显示每个 DHCP 报文的处理结果。最多保留最近 300 条。
  </span>
  <button onclick={onclear} disabled={packets.length === 0}>清空</button>
</div>

{#if packets.length === 0}
  <div class="empty">还没有报文。设备接上网线后这里会实时滚动。</div>
{:else}
  <div class="tablewrap">
    <table>
      <thead>
        <tr>
          <th>时间</th>
          <th>客户端</th>
          <th>请求</th>
          <th>结果</th>
          <th>地址</th>
          <th>说明</th>
        </tr>
      </thead>
      <tbody>
        {#each packets as p, i (`${p.at}-${i}-${p.client}`)}
          <tr>
            <td class="mono muted">{fmtClock(p.at)}</td>
            <td class="mono">{p.client}</td>
            <td class="mono">{p.request}</td>
            <td>
              <span class="pill {RESULT_STYLE[p.result] ?? ''}">{p.result}</span>
            </td>
            <td class="mono">{p.ip ?? '—'}</td>
            <td class="muted">{p.detail ?? ''}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}

<style>
  .bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 12px;
    font-size: 13px;
  }
</style>
