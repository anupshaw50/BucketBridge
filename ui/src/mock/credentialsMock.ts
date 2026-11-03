import type {
  AwsProfile,
  AwsProfileEntry,
  CredentialSource,
  InstanceRoleProbe,
  SsoAccount,
  SsoLoginPoll,
  SsoLoginStarted,
  SsoPortal,
  SsoSignInStatus,
} from "../api/types";
import { BackendError } from "../api/errors";
import type { BbErrorCode } from "../api/types";

function fail(code: BbErrorCode, message: string): never {
  throw new BackendError(code, message);
}

/** The profiles the mock "AWS files" hold. */
const PROFILES: AwsProfile[] = [
  profile("dev", "static_keys", { region: "eu-west-1" }),
  profile("prod-admin", "assume_role", {
    region: "us-east-1",
    role_arn: "arn:aws:iam::123456789012:role/Admin",
    source_profile: "dev",
  }),
  profile("corp-sso", "sso", {
    region: "eu-central-1",
    sso_start_url: "https://example.awsapps.com/start",
    sso_account_id: "123456789012",
    sso_role_name: "Admin",
  }),
  profile("broken", "incomplete", {
    importable: false,
    note: "the profile has no credentials, role, SSO or credential_process setting",
  }),
];

function profile(name: string, kind: AwsProfile["kind"], extra: Partial<AwsProfile>): AwsProfile {
  return {
    name,
    kind,
    region: null,
    role_arn: null,
    source_profile: null,
    sso_start_url: null,
    sso_account_id: null,
    sso_role_name: null,
    importable: true,
    note: null,
    ...extra,
  };
}

/** The accounts and roles of the mock portal. */
const SSO_ACCOUNTS: SsoAccount[] = [
  { account_id: "123456789012", account_name: "Production", email_address: "prod@example.com" },
  { account_id: "210987654321", account_name: "Sandbox", email_address: null },
];
const SSO_ROLES: Record<string, string[]> = {
  "123456789012": ["Admin", "ReadOnly"],
  "210987654321": ["Developer"],
};

/** Mock of the credential source commands: AWS profiles, the instance role and the SSO sign-in. */
export class CredentialsMock {
  /** Whether the machine has an instance role (the probe fails clearly when not). */
  instanceRoleAvailable = true;
  /** Polls that answer "pending" before a sign-in completes. */
  pendingPolls = 1;
  /** Seconds between polls the mock asks for. */
  intervalSecs = 1;
  private readonly signedIn = new Map<string, string>();
  private readonly logins = new Map<string, { portal: SsoPortal; polls: number }>();
  private sequence = 0;

  private readonly deps: { latency: () => Promise<void>; opener: (url: string) => void };

  constructor(deps: { latency: () => Promise<void>; opener: (url: string) => void }) {
    this.deps = deps;
  }

  /** The profiles, each with the account that links it (decided by the caller). */
  async listProfiles(linked: (profile: string) => string | null): Promise<AwsProfileEntry[]> {
    await this.deps.latency();
    return PROFILES.map((item) => ({ ...item, account_id: linked(item.name) }));
  }

  /** The profile named `name`, or undefined. */
  profile(name: string): AwsProfile | undefined {
    return PROFILES.find((item) => item.name === name);
  }

  async probeInstanceRole(): Promise<InstanceRoleProbe> {
    await this.deps.latency();
    if (!this.instanceRoleAvailable) {
      fail(
        "auth",
        "no instance or container role: the EC2 instance metadata service did not return credentials: this is not an EC2 instance, no role is attached to it, or the service is blocked",
      );
    }
    return { kind: "instance", access_key_id: "ASIAMOCKINSTANCE", expires_at: "2099-01-01T00:00:00Z" };
  }

  private static key(portal: SsoPortal): string {
    return portal.sso_session ? `session:${portal.sso_session}` : `url:${portal.start_url}`;
  }

  private static checkPortal(portal: SsoPortal): void {
    if (!portal.start_url.startsWith("https://")) {
      fail("usage", "the IAM Identity Center start URL must start with https://");
    }
    if (portal.sso_region.trim().length === 0) {
      fail("usage", "the region is empty");
    }
  }

  async startLogin(args: SsoPortal & { open_browser?: boolean }): Promise<SsoLoginStarted> {
    await this.deps.latency();
    CredentialsMock.checkPortal(args);
    this.sequence += 1;
    const loginId = `login-${this.sequence}`;
    this.logins.set(loginId, { portal: args, polls: 0 });
    const complete = "https://device.sso.example.com/?user_code=WDJB-MJHT";
    const open = args.open_browser ?? true;
    if (open) {
      this.deps.opener(complete);
    }
    return {
      login_id: loginId,
      user_code: "WDJB-MJHT",
      verification_uri: "https://device.sso.example.com/",
      verification_uri_complete: complete,
      expires_in_secs: 600,
      interval_secs: this.intervalSecs,
      browser_opened: open,
    };
  }

  async pollLogin(loginId: string): Promise<SsoLoginPoll> {
    await this.deps.latency();
    const login = this.logins.get(loginId);
    if (login === undefined) {
      fail("not_found", "there is no such sign-in in progress (it ended or expired): start again");
    }
    if (login.polls < this.pendingPolls) {
      login.polls += 1;
      return { state: "pending", interval_secs: this.intervalSecs };
    }
    this.logins.delete(loginId);
    const expires = new Date(Date.now() + 8 * 3600 * 1000).toISOString();
    this.signedIn.set(CredentialsMock.key(login.portal), expires);
    return { state: "complete", expires_at: expires };
  }

  cancelLogin(loginId: string): void {
    this.logins.delete(loginId);
  }

  async status(portal: SsoPortal): Promise<SsoSignInStatus> {
    await this.deps.latency();
    CredentialsMock.checkPortal(portal);
    const expires = this.signedIn.get(CredentialsMock.key(portal)) ?? null;
    return { signed_in: expires !== null, expires_at: expires, can_refresh: expires !== null };
  }

  async signOut(portal: SsoPortal): Promise<void> {
    await this.deps.latency();
    this.signedIn.delete(CredentialsMock.key(portal));
  }

  private requireSignedIn(portal: SsoPortal): void {
    CredentialsMock.checkPortal(portal);
    if (!this.signedIn.has(CredentialsMock.key(portal))) {
      fail("auth", "not signed in to IAM Identity Center (or the session ended): sign in again");
    }
  }

  async listAccounts(portal: SsoPortal): Promise<SsoAccount[]> {
    await this.deps.latency();
    this.requireSignedIn(portal);
    return SSO_ACCOUNTS.map((item) => ({ ...item }));
  }

  async listRoles(portal: SsoPortal, accountId: string): Promise<string[]> {
    await this.deps.latency();
    this.requireSignedIn(portal);
    return [...(SSO_ROLES[accountId] ?? [])];
  }

  /** Marks a portal as signed in, for tests that skip the browser. */
  signIn(portal: SsoPortal): void {
    this.signedIn.set(CredentialsMock.key(portal), new Date(Date.now() + 8 * 3600 * 1000).toISOString());
  }
}

/** Validates a credential source like the backend does when an account is saved. */
export function checkSource(
  source: CredentialSource,
  accounts: readonly { id: string }[],
  ownId: string | null,
): void {
  switch (source.type) {
    case "static_keys":
    case "instance_role":
      return;
    case "aws_profile":
      if (source.profile.trim().length === 0) {
        fail("usage", "the AWS profile name must be 1 to 128 characters without control characters");
      }
      return;
    case "assume_role": {
      if (!accounts.some((item) => item.id === source.source_account_id)) {
        fail("not_found", `the source account ${source.source_account_id} of the role does not exist`);
      }
      if (ownId !== null && ownId === source.source_account_id) {
        fail("usage", "an account cannot assume a role through itself");
      }
      if (!/^arn:aws[a-z-]*:iam::\d{12}:role\/.+/.test(source.role_arn)) {
        fail("usage", "the role ARN must look like arn:aws:iam::123456789012:role/Name");
      }
      if (!/^[\w+=,.@-]{2,64}$/.test(source.session_name)) {
        fail("usage", "the session name must be 2 to 64 letters, digits or any of _+=,.@-");
      }
      if (source.duration_secs < 900 || source.duration_secs > 43200) {
        fail("usage", "the role session length must be 900 to 43200 seconds");
      }
      return;
    }
    case "sso":
      if (!source.start_url.startsWith("https://")) {
        fail("usage", "the IAM Identity Center start URL must start with https://");
      }
      if (!/^\d{12}$/.test(source.account_id)) {
        fail("usage", "the AWS account id must be 12 digits");
      }
      if (source.role_name.trim().length === 0) {
        fail("usage", "the role name must be 1 to 64 characters");
      }
      return;
  }
}
