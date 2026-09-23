// The only script in the admin UI: one delegated listener that backs the
// copy-to-clipboard affordance on alias addresses.
document.addEventListener("click", async (event) => {
  const button = event.target.closest("[data-copy]");
  if (!button) return;

  try {
    await navigator.clipboard.writeText(button.dataset.copy);
  } catch {
    return;
  }

  const label = button.textContent;
  button.dataset.copied = "true";
  button.textContent = "copied";
  setTimeout(() => {
    delete button.dataset.copied;
    button.textContent = label;
  }, 1200);
});
