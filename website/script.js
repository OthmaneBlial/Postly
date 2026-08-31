(() => {
  document.body.classList.add("reveal-ready");
  const menuButton = document.querySelector(".menu-toggle");
  const nav = document.querySelector("#site-nav");

  menuButton?.addEventListener("click", () => {
    const isOpen = nav.classList.toggle("open");
    menuButton.setAttribute("aria-expanded", String(isOpen));
  });

  nav?.querySelectorAll("a").forEach((link) => {
    link.addEventListener("click", () => {
      nav.classList.remove("open");
      menuButton?.setAttribute("aria-expanded", "false");
    });
  });

  const copyButton = document.querySelector("[data-copy]");
  const copyStatus = document.querySelector(".copy-status");
  copyButton?.addEventListener("click", async () => {
    const command = copyButton.dataset.copy;
    if (!command) return;
    try {
      await navigator.clipboard.writeText(command);
      copyStatus.textContent = "Copied — ready for your local shell.";
      window.setTimeout(() => { copyStatus.textContent = ""; }, 2600);
    } catch {
      copyStatus.textContent = "Copy unavailable — use the README quick start.";
    }
  });

  const previewButtons = document.querySelectorAll("[data-preview-tab]");
  const previewPanels = document.querySelectorAll("[data-preview-panel]");
  previewButtons.forEach((button) => {
    button.addEventListener("click", () => {
      const tab = button.dataset.previewTab;
      previewButtons.forEach((item) => item.classList.toggle("active-response", item === button));
      previewPanels.forEach((panel) => panel.classList.toggle("active-panel", panel.dataset.previewPanel === tab));
    });
  });

  const docsFilter = document.querySelector("#docs-filter");
  const docsCards = [...document.querySelectorAll("[data-doc-card]")];
  const docsEmpty = document.querySelector("#docs-empty");
  docsFilter?.addEventListener("input", () => {
    const query = docsFilter.value.trim().toLowerCase();
    let visible = 0;
    docsCards.forEach((card) => {
      const searchable = `${card.dataset.docTitle || ""} ${card.textContent}`.toLowerCase();
      const matches = !query || searchable.includes(query);
      card.hidden = !matches;
      if (matches) visible += 1;
    });
    if (docsEmpty) docsEmpty.hidden = visible !== 0;
  });

  const revealItems = document.querySelectorAll(".reveal");
  if ("IntersectionObserver" in window) {
    const observer = new IntersectionObserver((entries, currentObserver) => {
      entries.forEach((entry) => {
        if (!entry.isIntersecting) return;
        entry.target.classList.add("visible");
        currentObserver.unobserve(entry.target);
      });
    }, { threshold: .12 });
    revealItems.forEach((item) => {
      if (item.getBoundingClientRect().top < window.innerHeight) {
        item.classList.add("visible");
      }
      observer.observe(item);
    });
  } else {
    revealItems.forEach((item) => item.classList.add("visible"));
  }
})();
