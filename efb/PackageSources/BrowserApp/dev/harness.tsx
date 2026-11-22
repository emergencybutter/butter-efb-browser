/**
 * Dev harness: mounts the real BrowserPage outside the sim (in Edge / Chrome),
 * with the Coherent API stubbed. Run `npm run harness`, start the companion,
 * then open dev/harness.html.
 */
import { FSComponent } from '@microsoft/msfs-sdk';
import { BrowserClient } from '../src/BrowserClient';
import { BrowserPage } from '../src/Components/BrowserPage';

const w = window as any;
if (!w.Coherent) {
  w.Coherent = {
    trigger: (...args: unknown[]): void => console.log('[Coherent.trigger]', ...args),
    on: (): void => undefined,
    off: (): void => undefined,
  };
}

const client = new BrowserClient();
w.browserClient = client;
FSComponent.render(<BrowserPage appViewService={{} as any} client={client} />, document.getElementById('root'));
client.resume();
