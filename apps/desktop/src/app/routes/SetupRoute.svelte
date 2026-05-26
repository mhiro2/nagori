<script lang="ts">
  import PermissionCard from '../components/PermissionCard.svelte';
  import { messages } from '../lib/i18n/index.svelte';

  const t = $derived(messages());
</script>

<section class="setup">
  <header>
    <h1>{t.setup.title}</h1>
    <p class="intro">{t.setup.intro}</p>
  </header>
  <div class="cards">
    <PermissionCard kind="accessibility" />
  </div>
</section>

<style>
  .setup {
    display: grid;
    gap: 1rem;
    padding: 0.5rem 0 1rem;
  }
  header {
    display: grid;
    gap: 0.35rem;
  }
  h1 {
    margin: 0;
    font-size: 1.1rem;
    font-weight: 600;
  }
  .intro {
    margin: 0;
    color: var(--muted, rgba(255, 255, 255, 0.7));
    font-size: 0.85rem;
    line-height: 1.45;
  }
  .cards {
    display: grid;
    gap: 0.75rem;
  }
</style>
