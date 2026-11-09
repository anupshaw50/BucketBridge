import { setLocale } from "../i18n";

/**
 * The tests run in English, the default language. This module is imported
 * first by `setup.ts`, before the test file and the application modules are
 * loaded, so the labels that a module builds once (tables of choices, column
 * titles) are built in English too. Tests that cover the Chinese interface
 * switch with `setLocale("zh-CN")` themselves.
 */
setLocale("en");
