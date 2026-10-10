// The copy button next to the install command.
document.addEventListener("click", (event) => {
  const button = event.target.closest(".copy-btn");
  const command = button && button.closest(".install-box").querySelector(".cmd");
  if (!command || !navigator.clipboard) return;
  const label = button.querySelector("span");
  navigator.clipboard.writeText(command.textContent.replace(/^\$\s*/, "")).then(() => {
    const before = label.textContent;
    label.textContent = button.dataset.copied || "Copied";
    setTimeout(() => (label.textContent = before), 1500);
  });
});
