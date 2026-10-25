// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

export interface TeamInfo {
  id: string;
  name: string;
  created_at: string;
  member_count: number;
  auto_join_domain?: string | null;
}

export interface TeamMember {
  user_id: string;
  role: "admin" | "member";
  joined_at: string;
  // populated by backend when /api/team/members joins the users table
  name?: string | null;
  email?: string | null;
}

export interface TeamConfig {
  id: string;
  scope: string;
  config_type: string;
  key: string;
  value_encrypted: string;
  nonce: string;
  updated_at: string;
  updated_by: string;
  // decrypted locally
  value?: object;
}

interface TeamState {
  team: TeamInfo | null;
  role: "admin" | "member" | null;
  members: TeamMember[];
  configs: TeamConfig[];
  inviteLink: string | null;
  invitePassphrase: string | null;
  /** True when user is in a team but encryption key is not on this device */
  missingKey: boolean;
  /** True once /configs has been fetched successfully at least once.
   * Consumers that react to a config DISAPPEARING (e.g. disabling
   * unshared team pipes) must gate on this — an empty `configs` from a
   * failed fetch must never look like "everything was unshared". */
  configsFetched: boolean;
  loading: boolean;
  error: string | null;
}

const LOCAL_TEAM_STATE: TeamState = {
  team: null, role: null, members: [], configs: [], inviteLink: null,
  invitePassphrase: null, missingKey: false, configsFetched: true,
  loading: false, error: null,
};

const unavailable = async (..._args: unknown[]): Promise<any> => {
  throw new Error("Team services are not part of this personal edition.");
};
const refresh = async () => {};

/** Compatibility for shared preset/task views; no account or team transport. */
export function useTeam() {
  return {
    ...LOCAL_TEAM_STATE,
    fetchTeam: refresh,
    fetchConfigs: refresh,
    createTeam: unavailable,
    initializeTeamKey: unavailable,
    joinTeam: unavailable,
    leaveTeam: unavailable,
    deleteTeam: unavailable,
    removeMember: unavailable,
    pushConfig: unavailable,
    pushConfigPlain: unavailable,
    deleteConfig: unavailable,
    refreshInviteLink: unavailable,
    generateInviteToken: unavailable,
  };
}
