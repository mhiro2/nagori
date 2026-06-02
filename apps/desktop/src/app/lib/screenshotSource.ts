// Recognise the source app of an image clip as a screenshot tool, so an
// image result row can hint "this came from a screenshot" — the single most
// common way a developer's clipboard fills with images. Name-based and
// best-effort: the source app display name is all the capture pipeline
// records, so this is a substring match against the well-known tools across
// macOS / Windows / Linux. Conservative on purpose — a missed hint is fine,
// a wrong one is noise.

const SCREENSHOT_APP_MARKERS: readonly string[] = [
  'screenshot', // macOS Screenshot.app, GNOME Screenshot, Windows "Screenshot"
  'screencapture', // macOS screencapture(1)
  'screen capture',
  'snipping tool', // Windows
  'snip & sketch', // Windows
  'cleanshot', // CleanShot X
  'shottr',
  'flameshot', // Linux
  'spectacle', // KDE
  'ksnip',
  'greenshot',
  'lightshot',
  'sharex', // Windows
  'snagit',
  'skitch',
  'xfce4-screenshooter',
  'gnome-screenshot',
];

// Whether the given source app display name looks like a screenshot tool.
export const isScreenshotSource = (sourceAppName: string | null | undefined): boolean => {
  if (!sourceAppName) return false;
  const lower = sourceAppName.toLowerCase();
  return SCREENSHOT_APP_MARKERS.some((marker) => lower.includes(marker));
};
