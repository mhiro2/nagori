// Strong-brand domain → short label, surfaced as a badge on URL result rows
// so a history of bare links is easier to scan ("which GitHub link was it?").
//
// Hostname only — no network fetch, ever. The match is deliberately narrow:
// only well-known brands are labelled; an unrecognised host shows just its
// domain, because a wrong category badge ("docs"/"dev" guesses) is worse than
// none. Labels are proper nouns, so they are intentionally not translated.

type BrandRule = {
  // Registrable domains this brand owns. A host matches when it equals one of
  // these or is a subdomain of it (`gist.github.com` → `github.com`).
  hosts: readonly string[];
  label: string;
};

// Ordered most-specific-first: subdomain-scoped brands (docs/drive.google.com)
// must be tested before the bare registrable domain (google.com) they sit
// under, otherwise the broad rule would shadow them.
const BRAND_RULES: readonly BrandRule[] = [
  { hosts: ['docs.google.com'], label: 'Google Docs' },
  { hosts: ['drive.google.com'], label: 'Google Drive' },
  { hosts: ['mail.google.com'], label: 'Gmail' },
  { hosts: ['github.com', 'githubusercontent.com', 'github.io'], label: 'GitHub' },
  { hosts: ['gitlab.com'], label: 'GitLab' },
  { hosts: ['bitbucket.org'], label: 'Bitbucket' },
  { hosts: ['youtube.com', 'youtu.be'], label: 'YouTube' },
  { hosts: ['stackoverflow.com'], label: 'Stack Overflow' },
  { hosts: ['stackexchange.com'], label: 'Stack Exchange' },
  { hosts: ['notion.so', 'notion.site'], label: 'Notion' },
  { hosts: ['slack.com'], label: 'Slack' },
  { hosts: ['figma.com'], label: 'Figma' },
  { hosts: ['linear.app'], label: 'Linear' },
  { hosts: ['atlassian.net'], label: 'Atlassian' },
  { hosts: ['reddit.com'], label: 'Reddit' },
  { hosts: ['twitter.com', 'x.com'], label: 'X' },
  { hosts: ['npmjs.com'], label: 'npm' },
  { hosts: ['crates.io'], label: 'crates.io' },
  { hosts: ['developer.mozilla.org'], label: 'MDN' },
  { hosts: ['wikipedia.org'], label: 'Wikipedia' },
  { hosts: ['medium.com'], label: 'Medium' },
  { hosts: ['zenn.dev'], label: 'Zenn' },
  { hosts: ['qiita.com'], label: 'Qiita' },
  { hosts: ['google.com'], label: 'Google' },
];

// Normalise a host for matching: lower-case and drop a trailing FQDN dot.
// `URL.hostname` already strips the port and userinfo, so this is all we need.
const normalizeHost = (host: string): string => host.toLowerCase().replace(/\.$/, '');

const matchesHost = (host: string, owned: string): boolean =>
  host === owned || host.endsWith(`.${owned}`);

// Return the brand label for a host, or `undefined` when it is not a known
// brand (the caller shows the plain domain instead).
export const domainCategory = (host: string | null | undefined): string | undefined => {
  if (!host) return undefined;
  const normalized = normalizeHost(host);
  if (normalized === '') return undefined;
  for (const rule of BRAND_RULES) {
    if (rule.hosts.some((owned) => matchesHost(normalized, owned))) return rule.label;
  }
  return undefined;
};
