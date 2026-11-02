(() => {
  'use strict';
  const HOST_ORIGIN = __HOST_ORIGIN__;
  const VIEW_URL = __VIEW_URL__;
  const view = document.getElementById('view');
  let appWindow = null;
  let loaded = false;
  const validJsonRpc = message => message && typeof message === 'object' && message.jsonrpc === '2.0'

  window.addEventListener('message', event => {
    if (event.source === window.parent && event.origin === HOST_ORIGIN) {
      const message = event.data;
      if (!validJsonRpc(message)) return;
      if (message.method === 'ui/notifications/sandbox-resource-ready') {
        // The host-provided HTML is deliberately ignored. Native serves immutable
        // HTML at the per-widget URL using an independently constructed CSP.
        if (!loaded) {
          loaded = true;
          // Capture the stable WindowProxy before navigation. Module scripts in
          // the child can post initialize before its load event fires.
          appWindow = view.contentWindow;
          view.src = VIEW_URL;
        }
        return;
      }
      if (appWindow) appWindow.postMessage(message, location.origin);
      return;
    }
    if (appWindow && event.source === appWindow && event.origin === location.origin && validJsonRpc(event.data)) {
      window.parent.postMessage(event.data, HOST_ORIGIN);
    }
  });

  window.parent.postMessage({ jsonrpc: '2.0', method: 'ui/notifications/sandbox-proxy-ready', params: {} }, HOST_ORIGIN);
})();
