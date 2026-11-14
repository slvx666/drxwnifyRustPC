<script lang="ts">
  // Milestone 3: an almost empty Full shell (player bar only) proving the core works over Tauri IPC.
  // The real UI with all themes, looks and animations is milestone 9.
  import { core, type LibraryCounts } from "./core";

  let counts = $state<LibraryCounts | null>(null);
  let error = $state("");
  core.libraryCounts().then((c) => (counts = c)).catch((e) => (error = String(e)));
</script>

<main>
  <section class="content">
    <h1>drxwnify</h1>
    {#if counts}
      <p>{counts.liked} любимых треков · {counts.playlists} плейлистов · {counts.history_events} прослушиваний</p>
    {:else if error}
      <p class="err">{error}</p>
    {:else}
      <p>Загрузка…</p>
    {/if}
  </section>
  <footer class="player">
    <div class="cover"></div>
    <div class="meta"><b>Ничего не играет</b><span>Drxwnify Full</span></div>
    <button class="play" aria-label="Играть">▶</button>
  </footer>
</main>

<style>
  :global(html, body) {
    margin: 0;
    height: 100%;
    background: #09080a;
    color: #f3eaed;
    font-family: Manrope, system-ui, sans-serif;
  }
  main { display: flex; flex-direction: column; height: 100vh; }
  .content { flex: 1; padding: 24px 28px; }
  h1 { margin: 0 0 8px; font-weight: 800; letter-spacing: -0.4px; }
  .err { color: #e0566f; }
  .player {
    display: flex; align-items: center; gap: 14px; height: 76px; margin: 0 12px 12px; padding: 0 16px;
    background: rgba(255, 255, 255, 0.06); border: 1px solid rgba(255, 255, 255, 0.08); border-radius: 20px;
  }
  .cover { width: 52px; height: 52px; border-radius: 12px; background: rgba(255, 255, 255, 0.09); }
  .meta { display: flex; flex-direction: column; flex: 1; }
  .meta span { color: #ab9fa5; font-size: 12px; }
  .play { width: 42px; height: 42px; border-radius: 50%; border: 0; background: #f4a7bb; color: #2a1220; font-size: 16px; cursor: pointer; }
</style>
