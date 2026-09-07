(() => {
  const status = document.querySelector('.copy-status');
  let clearStatus;
  document.querySelectorAll('[data-copy-target]').forEach((button) => {
    button.addEventListener('click', async () => {
      const source = document.getElementById(button.dataset.copyTarget);
      if (!source) return;
      try {
        await navigator.clipboard.writeText(source.textContent.trim());
        status.textContent = 'Copied. Ready for your terminal.';
      } catch {
        status.textContent = 'Clipboard unavailable. Select and copy the command above.';
      }
      clearTimeout(clearStatus);
      clearStatus = setTimeout(() => { status.textContent = ''; }, 4000);
    });
  });
  const filter = document.getElementById('docs-filter');
  const guides = [...document.querySelectorAll('[data-guide]')];
  filter?.addEventListener('input', () => {
    const query = filter.value.trim().toLowerCase();
    for (const guide of guides) guide.hidden = !guide.textContent.toLowerCase().includes(query);
    document.getElementById('docs-empty').hidden = guides.some((guide) => !guide.hidden);
  });
})();
