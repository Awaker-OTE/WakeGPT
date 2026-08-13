import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const [appCss, cardScript] = await Promise.all([
  readFile(new URL("../src/App.css", import.meta.url), "utf8"),
  readFile(new URL("../src-tauri/assets/codex_card_v1.js", import.meta.url), "utf8"),
]);

const capture = (source, pattern, label) => {
  const value = source.match(pattern)?.[1];
  assert.ok(value, `${label} must exist`);
  return value;
};

const variables = (block) => Object.fromEntries(
  [...block.matchAll(/--([\w-]+):\s*(#[0-9a-f]{3,6}|rgba?\([^;)]+\))\s*;/giu)]
    .map((match) => [match[1], match[2]]),
);

const linearChannel = (channel) => {
  const value = channel / 255;
  return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
};

const color = (value) => {
  if (value.startsWith("#")) {
    const normalized = value.slice(1).length === 3
      ? [...value.slice(1)].map((character) => character.repeat(2)).join("")
      : value.slice(1);
    const [red, green, blue] = normalized.match(/../gu).map((channel) => parseInt(channel, 16));
    return { red, green, blue, alpha: 1 };
  }
  const channels = value.match(/[\d.]+/gu)?.map(Number) ?? [];
  assert.ok(channels.length === 3 || channels.length === 4, `unsupported color ${value}`);
  const [red, green, blue, alpha = 1] = channels;
  return { red, green, blue, alpha };
};

const opaqueForeground = (foreground, background) => {
  const front = color(foreground);
  const back = color(background);
  assert.equal(back.alpha, 1, `background ${background} must be opaque`);
  return {
    red: front.red * front.alpha + back.red * (1 - front.alpha),
    green: front.green * front.alpha + back.green * (1 - front.alpha),
    blue: front.blue * front.alpha + back.blue * (1 - front.alpha),
  };
};

const luminance = ({ red, green, blue }) => {
  return 0.2126 * linearChannel(red)
    + 0.7152 * linearChannel(green)
    + 0.0722 * linearChannel(blue);
};

const contrast = (foreground, background) => {
  const first = luminance(opaqueForeground(foreground, background));
  const second = luminance(color(background));
  return (Math.max(first, second) + 0.05) / (Math.min(first, second) + 0.05);
};

const requireVariable = (theme, name, themeName) => {
  const value = theme[name];
  assert.ok(value, `${themeName} must define --${name}`);
  return value;
};

const assertTextContrast = (theme, foreground, backgrounds, themeName) => {
  const foregroundValue = requireVariable(theme, foreground, themeName);
  for (const background of backgrounds) {
    const backgroundValue = requireVariable(theme, background, themeName);
    assert.ok(
      contrast(foregroundValue, backgroundValue) >= 4.5,
      `${themeName} --${foreground} on --${background} must meet 4.5:1`,
    );
  }
};

const assertContrastIncreases = (regular, enhanced, foregrounds, backgrounds, themeName) => {
  for (const foreground of foregrounds) {
    for (const background of backgrounds) {
      const regularRatio = contrast(
        requireVariable(regular, foreground, `${themeName} regular`),
        requireVariable(regular, background, `${themeName} regular`),
      );
      const enhancedRatio = contrast(
        requireVariable(enhanced, foreground, `${themeName} enhanced`),
        requireVariable(enhanced, background, `${themeName} enhanced`),
      );
      assert.ok(
        enhancedRatio > regularRatio,
        `${themeName} --${foreground} on --${background} must gain contrast when requested`,
      );
    }
  }
};

const appLightBlock = capture(appCss, /:root\s*\{([^}]*)\}/u, "desktop light theme");
const appDarkBlock = capture(
  appCss,
  /:root\[data-theme="dark"\]\s*\{([^}]*)\}/u,
  "desktop dark theme",
);
const appSystemDarkBlock = capture(
  appCss,
  /@media \(prefers-color-scheme:\s*dark\)\s*\{\s*:root:not\(\[data-theme\]\)\s*\{([^}]*)\}/u,
  "desktop system dark theme",
);
assert.match(appLightBlock, /color-scheme:\s*light/u);
assert.match(appDarkBlock, /color-scheme:\s*dark/u);
assert.match(appSystemDarkBlock, /color-scheme:\s*dark/u);

for (const [name, theme] of [
  ["desktop light theme", variables(appLightBlock)],
  ["desktop dark theme", variables(appDarkBlock)],
  ["desktop system dark theme", variables(appSystemDarkBlock)],
]) {
  const surfaces = [
    "background",
    "sidebar",
    "surface",
    "surface-subtle",
    "document-surface",
    "code-surface",
  ];
  for (const foreground of ["text", "text-muted", "text-faint"]) {
    assertTextContrast(theme, foreground, surfaces, name);
  }
  for (const [foreground, background] of [
    ["danger", "danger-soft"],
    ["success", "success-soft"],
    ["warning", "warning-soft"],
  ]) {
    assertTextContrast(theme, foreground, [background], name);
  }
}

assert.deepEqual(
  variables(appSystemDarkBlock),
  variables(appDarkBlock),
  "following the system dark appearance must use the same verified tokens as explicit dark mode",
);

const appContrastLightBlock = capture(
  appCss,
  /@media \(prefers-contrast:\s*more\)\s*\{\s*:root\s*\{([^}]*)\}/u,
  "desktop increased-contrast light theme",
);
const appContrastDarkBlock = capture(
  appCss,
  /@media \(prefers-contrast:\s*more\)\s*\{[\s\S]*?:root\[data-theme="dark"\]\s*\{([^}]*)\}/u,
  "desktop increased-contrast explicit dark theme",
);
const appContrastSystemDarkBlock = capture(
  appCss,
  /@media \(prefers-color-scheme:\s*dark\) and \(prefers-contrast:\s*more\)\s*\{\s*:root:not\(\[data-theme\]\)\s*\{([^}]*)\}/u,
  "desktop increased-contrast system dark theme",
);
const enhancedAppThemes = [
  ["desktop light theme", variables(appLightBlock), variables(appContrastLightBlock)],
  ["desktop dark theme", variables(appDarkBlock), variables(appContrastDarkBlock)],
  ["desktop system dark theme", variables(appSystemDarkBlock), variables(appContrastSystemDarkBlock)],
].map(([name, regular, overrides]) => [name, regular, { ...regular, ...overrides }]);

for (const [name, regular, enhanced] of enhancedAppThemes) {
  assertContrastIncreases(
    regular,
    enhanced,
    ["text-muted", "text-faint", "border", "border-strong"],
    ["surface", "surface-subtle"],
    `${name} increased contrast`,
  );
}
assert.deepEqual(
  variables(appContrastSystemDarkBlock),
  variables(appContrastDarkBlock),
  "increased contrast must use the same overrides for explicit and system dark modes",
);
assert.match(
  appCss,
  /@media \(prefers-contrast:\s*more\)[\s\S]*button:focus-visible[\s\S]*outline-width:\s*3px/u,
  "desktop focus indicators must become thicker when increased contrast is requested",
);

const cardLightBlock = capture(cardScript, /:host\s*\{([^}]*)\}/u, "card light theme");
const cardDarkBlock = capture(
  cardScript,
  /@media \(prefers-color-scheme:dark\)\s*\{\s*:host\s*\{([^}]*)\}/u,
  "card dark theme",
);
const cardContrastLightBlock = capture(
  cardScript,
  /@media \(prefers-contrast:more\)\s*\{\s*:host\s*\{([^}]*)\}/u,
  "card increased-contrast light theme",
);
const cardContrastDarkBlock = capture(
  cardScript,
  /@media \(prefers-color-scheme:dark\) and \(prefers-contrast:more\)\s*\{\s*:host\s*\{([^}]*)\}/u,
  "card increased-contrast dark theme",
);
assert.match(cardLightBlock, /color-scheme:light/u);
assert.match(cardDarkBlock, /color-scheme:dark/u);

for (const [name, theme] of [
  ["card light theme", variables(cardLightBlock)],
  ["card dark theme", variables(cardDarkBlock)],
]) {
  for (const foreground of ["wake-text", "wake-muted"]) {
    assertTextContrast(theme, foreground, ["wake-surface", "wake-input", "wake-subtle"], name);
  }
}

for (const [name, regular, overrides] of [
  ["card light theme", variables(cardLightBlock), variables(cardContrastLightBlock)],
  ["card dark theme", variables(cardDarkBlock), variables(cardContrastDarkBlock)],
]) {
  const enhanced = { ...regular, ...overrides };
  assertContrastIncreases(
    regular,
    enhanced,
    ["wake-muted", "wake-border"],
    ["wake-surface", "wake-input", "wake-subtle"],
    `${name} increased contrast`,
  );
}
assert.match(
  cardScript,
  /@media \(prefers-contrast:more\)[\s\S]*button:focus-visible[\s\S]*outline-width:3px/u,
  "card focus indicators must become thicker when increased contrast is requested",
);

console.log("theme contrast contract: ok");
