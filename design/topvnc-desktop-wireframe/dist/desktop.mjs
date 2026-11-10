const $ = selector => document.querySelector(selector);
function size(value) {
  document.body.dataset.desktopSize = value;
  document.querySelectorAll('[data-desktop-size]').forEach(button => {
    const selected = button.dataset.desktopSize === value;
    button.classList.toggle('selected', selected);
    button.setAttribute('aria-pressed', String(selected));
  });
  $('#dimension-label').textContent = value === 'compact' ? 'COMPACT LAPTOP / RESPONSIVE PREVIEW' : 'DESKTOP / RESPONSIVE PREVIEW';
}
document.querySelectorAll('[data-desktop-size]').forEach(button => button.addEventListener('click', () => size(button.dataset.desktopSize)));
$('#try-rotate').addEventListener('click', () => size(document.body.dataset.desktopSize === 'compact' ? 'wide' : 'compact'));
size('wide');

// Native browser editing simulates physical desktop typing; no host is involved.
const output = $('#remote-output');
output.contentEditable = 'plaintext-only';
output.setAttribute('role', 'textbox');
output.setAttribute('aria-label', 'Simulated remote text editor');
output.setAttribute('aria-multiline', 'true');
output.addEventListener('input', () => {
  if (output.textContent.length > 2000) output.textContent = output.textContent.slice(0, 2000);
});
$('#desktop-fullscreen').addEventListener('click', () => $('#focus-preview').click());
$('#desktop-fit').addEventListener('click', () => {
  size('wide');
  output.focus({ preventScroll: true });
});
