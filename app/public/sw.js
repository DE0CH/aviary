// The old CloudCLI UI registered this service worker. The staff UI is now the Paseo web app, which has none: this
// version clears the old caches, unregisters itself and reloads the open tabs onto the new UI.
self.addEventListener('install', () => self.skipWaiting());

self.addEventListener('activate', (event) => {
  event.waitUntil((async () => {
    for (const key of await caches.keys()) await caches.delete(key);
    await self.registration.unregister();
    const tabs = await self.clients.matchAll({ type: 'window' });
    for (const tab of tabs) tab.navigate(tab.url);
  })());
});
