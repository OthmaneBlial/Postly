(() => {
  const status = document.querySelector('.copy-status');
  document.querySelectorAll('[data-video-time]').forEach((button) => {
    button.addEventListener('click', async () => {
      const video = button.closest('section').querySelector('video');
      try {
        video.currentTime = Number(button.dataset.videoTime);
        await video.play();
      } catch {
        status.textContent = 'Use the video controls to start playback.';
      }
    });
  });
  document.querySelectorAll('[data-video-fullscreen]').forEach((button) => {
    button.addEventListener('click', async () => {
      const video = button.closest('section').querySelector('video');
      try {
        if (video.requestFullscreen) await video.requestFullscreen();
        else if (video.webkitEnterFullscreen) video.webkitEnterFullscreen();
        else status.textContent = 'Use your browser’s video controls for full screen.';
      } catch {
        status.textContent = 'Full screen is unavailable in this browser. The inline player still works.';
      }
    });
  });
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
