//! Plural tables + the one/few/many picker (AC-301 split): pure data plus the
//! locale rule; `tp` stays in `strings.ts` (the facade) and consumes these.
//! RU: 1 -> one, 2-4 -> few, 0/5-20 (incl. 11-14) -> many. EN: 1 -> one, else many.

import type { Locale } from "../strings";

export type PluralForm = "one" | "few" | "many";

export function pluralForm(locale: Locale, n: number): PluralForm {
  if (locale === "ru") {
    const mod10 = Math.abs(n) % 10;
    const mod100 = Math.abs(n) % 100;
    if (mod10 === 1 && mod100 !== 11) return "one";
    if (mod10 >= 2 && mod10 <= 4 && (mod100 < 12 || mod100 > 14)) return "few";
    return "many";
  }
  return n === 1 ? "one" : "many";
}

export const enPlural = {
  files: { one: "{n} file", few: "{n} files", many: "{n} files" },
  filesChanged: { one: "{n} file changed", few: "{n} files changed", many: "{n} files changed" },
  entries: { one: "{n} entry", few: "{n} entries", many: "{n} entries" },
  servers: { one: "{n} server", few: "{n} servers", many: "{n} servers" },
  groups: { one: "{n} group", few: "{n} groups", many: "{n} groups" },
  runs: { one: "{n} run", few: "{n} runs", many: "{n} runs" },
  steps: { one: "{n} step", few: "{n} steps", many: "{n} steps" },
  toolCalls: { one: "{n} tool call", few: "{n} tool calls", many: "{n} tool calls" },
  changedFiles: { one: "{n} file changed", few: "{n} files changed", many: "{n} files changed" },
  models: { one: "{n} model", few: "{n} models", many: "{n} models" },
  commits: { one: "{n} recent commit", few: "{n} recent commits", many: "{n} recent commits" },
} as const;

export type PluralKey = keyof typeof enPlural;

export type PluralTable = Record<PluralKey, Record<PluralForm, string>>;

export const ruPlural: PluralTable = {
  files: { one: "{n} файл", few: "{n} файла", many: "{n} файлов" },
  filesChanged: { one: "{n} файл изменён", few: "{n} файла изменено", many: "{n} файлов изменено" },
  entries: { one: "{n} запись", few: "{n} записи", many: "{n} записей" },
  servers: { one: "{n} сервер", few: "{n} сервера", many: "{n} серверов" },
  groups: { one: "{n} группа", few: "{n} группы", many: "{n} групп" },
  runs: { one: "{n} запуск", few: "{n} запуска", many: "{n} запусков" },
  steps: { one: "{n} шаг", few: "{n} шага", many: "{n} шагов" },
  toolCalls: { one: "{n} вызов инструмента", few: "{n} вызова инструментов", many: "{n} вызовов инструментов" },
  changedFiles: { one: "{n} файл изменён", few: "{n} файла изменено", many: "{n} файлов изменено" },
  models: { one: "{n} модель", few: "{n} модели", many: "{n} моделей" },
  commits: { one: "{n} недавний коммит", few: "{n} недавних коммита", many: "{n} недавних коммитов" },
};

