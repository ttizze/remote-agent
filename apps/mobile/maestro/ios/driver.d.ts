declare const AGENT_IOS_DRIVER_URL: string;
declare const AGENT_PAIRING_URL: string;
declare const REFRESH_PAIRING: string;
declare const ABSENT_ID: string | undefined;
declare const ABSENT_ELEMENT_TYPE: string | undefined;
declare const output: { pairingInvitation?: string };
declare const http: {
  get(url: string): { status: number; body: string };
  post(url: string, options: { headers: Record<string, string>; body: string }): { status: number; body: string };
};
