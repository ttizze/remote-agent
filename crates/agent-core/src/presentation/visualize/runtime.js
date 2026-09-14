(() => {
  const notice = message => {
    let element = document.getElementById('bex-runtime-notice');
    if (!element) {
      element = document.createElement('p');
      element.id = 'bex-runtime-notice';
      element.setAttribute('role', 'status');
      document.body.prepend(element);
    }
    element.textContent = message;
  };
  const unsupported = () => {
    notice('このCodex専用操作はBexでは利用できません。会話の入力欄から依頼してください。');
    return Promise.reject(new Error('Codex host API is unavailable in Bex'));
  };
  window.openai = new Proxy({}, { get: () => unsupported });
  window.addEventListener('error', () => notice('表示の一部を実行できません。外部ライブラリやCodex専用機能への依存を確認してください。'));
  window.addEventListener('unhandledrejection', () => notice('この操作はBexでは利用できません。会話の入力欄から依頼してください。'));
  document.addEventListener('securitypolicyviolation', () => notice('外部通信・追加ファイルの読み込みは無効です。必要なデータを含む表示を依頼してください。'));
  document.addEventListener('DOMContentLoaded', () => lucide.createIcons());
})();
