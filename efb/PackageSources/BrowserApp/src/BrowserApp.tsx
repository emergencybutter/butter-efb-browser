import {
  App,
  AppBootMode,
  AppInstallProps,
  AppSuspendMode,
  AppView,
  AppViewProps,
  Efb,
  RequiredProps,
  TVNode,
} from '@efb/efb-api';
import { FSComponent, VNode } from '@microsoft/msfs-sdk';
import { BrowserClient } from './BrowserClient';
import { BrowserPage } from './Components/BrowserPage';

import './BrowserApp.scss';

/** Defined in build.js: points to the app's dist folder inside the sim VFS. */
declare const BASE_URL: string;

class BrowserAppView extends AppView<RequiredProps<AppViewProps, 'bus'>> {
  protected defaultView = 'Browser';

  private readonly client = new BrowserClient();

  protected registerViews(): void {
    this.appViewService.registerPage('Browser', () => (
      <BrowserPage appViewService={this.appViewService} client={this.client} />
    ));
  }

  public onOpen(): void {
    this.client.resume();
  }

  public onResume(): void {
    this.client.resume();
  }

  public onPause(): void {
    this.client.pause();
  }

  public onClose(): void {
    this.client.pause();
  }

  public destroy(): void {
    this.client.destroy();
    super.destroy();
  }

  public render(): VNode {
    return <div class="browser-app">{super.render()}</div>;
  }
}

class BrowserApp extends App {
  public get name(): string {
    return 'Browser';
  }

  public get icon(): string {
    return `${BASE_URL}/Assets/app-icon.svg`;
  }

  public BootMode = AppBootMode.COLD;

  /** Keep the page alive in the background so switching apps does not reconnect. */
  public SuspendMode = AppSuspendMode.SLEEP;

  public async install(_props: AppInstallProps): Promise<void> {
    Efb.loadCss(`${BASE_URL}/BrowserApp.css`);
  }

  /** Available on every aircraft. */
  public get compatibleAircraftModels(): string[] | undefined {
    return undefined;
  }

  public render(): TVNode<BrowserAppView> {
    return <BrowserAppView bus={this.bus} />;
  }
}

Efb.use(BrowserApp);
