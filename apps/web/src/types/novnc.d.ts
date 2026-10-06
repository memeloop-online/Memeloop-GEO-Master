declare module "@novnc/novnc" {
  export default class RFB {
    constructor(
      target: HTMLElement,
      url: string,
      options: { wsProtocols: string[] },
    );
    scaleViewport: boolean;
    resizeSession: boolean;
    viewOnly: boolean;
    addEventListener(
      type: "connect" | "disconnect" | "securityfailure",
      listener: (event: Event) => void,
    ): void;
    disconnect(): void;
    focus(): void;
    clipboardPasteFrom(text: string): void;
  }
}
