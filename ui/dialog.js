// One native confirmation dialog for destructive or surprising actions.
// Escape and the backdrop cancel; the safe choice keeps focus.

let wired = false;

function elements() {
  const dialog = document.getElementById('confirm');
  return {
    dialog,
    title: document.getElementById('confirm-title'),
    text: document.getElementById('confirm-text'),
    ok: document.getElementById('confirm-ok'),
    cancel: document.getElementById('confirm-cancel'),
  };
}

/** Resolves `true` only when the learner confirms. */
export function confirmDialog({
  title = 'Are you sure?',
  text = '',
  confirmLabel = 'Continue',
  cancelLabel = 'Cancel',
} = {}) {
  const { dialog, title: titleEl, text: textEl, ok, cancel } = elements();
  if (!dialog) return Promise.resolve(false);
  titleEl.textContent = title;
  textEl.textContent = text;
  textEl.hidden = !text;
  ok.textContent = confirmLabel;
  cancel.textContent = cancelLabel;

  if (!wired) {
    wired = true;
    // Backdrop clicks close the dialog (the same path as Cancel).
    dialog.addEventListener('click', (e) => {
      if (e.target === dialog) dialog.close('cancel');
    });
  }
  return new Promise((resolve) => {
    const done = (value) => {
      ok.removeEventListener('click', accept);
      cancel.removeEventListener('click', decline);
      dialog.removeEventListener('close', onClose);
      if (dialog.open) dialog.close();
      resolve(value);
    };
    const accept = () => done(true);
    const decline = () => done(false);
    const onClose = () => done(false);
    ok.addEventListener('click', accept);
    cancel.addEventListener('click', decline);
    dialog.addEventListener('close', onClose);
    if (dialog.open) dialog.close();
    dialog.showModal();
    cancel.focus({ preventScroll: true });
  });
}
