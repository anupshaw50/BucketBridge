import { Anchor, Button, Code, Divider, Group, ScrollArea, Stack, Table, Text, Title } from "@mantine/core";
import { IconBook, IconBrandGithub, IconBrandLinkedin, IconBug, IconHeart, IconRefresh, IconUser, IconWorld } from "@tabler/icons-react";
// The full GPL-3.0 text, bundled from the repository root at build time.
import LICENSE_TEXT from "../../../../LICENSE?raw";
import { useModel, useSlice } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { BrandLogo } from "../../components/BrandLogo";
import { tr } from "../../i18n";

export const REPOSITORY_URL = "https://github.com/anupshaw50/BucketBridge";
export const WEBSITE_URL = "https://anupshaw50.github.io/BucketBridge/";
export const AUTHOR_NAME = "Anup Shaw";
export const AUTHOR_HANDLE = "anupshaw50";
export const AUTHOR_URL = "https://github.com/anupshaw50";
export const LINKEDIN_URL = "https://www.linkedin.com/in/anup-kumar-shaw/";
export const SPONSOR_URL = "https://buymeacoffee.com/anupshaw50b";

const COPYRIGHT_NOTICE = `BucketBridge
Copyright (C) 2026 Anup Shaw (anupshaw50)

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU General Public License as published by the Free Software
Foundation, version 3.

This program is distributed in the hope that it will be useful, but WITHOUT
ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
FOR A PARTICULAR PURPOSE. See the GNU General Public License for more details.

You should have received a copy of the GNU General Public License along with
this program (the LICENSE file next to it). If not, see
<https://www.gnu.org/licenses/>.`;

/** Version, license, links, and the statement that nothing needs activating. */
export function AboutDialog({ onClose }: { onClose: () => void }) {
  const model = useModel();
  const info = useSlice(model.app, (state) => state.info);
  // The desktop shell only opens the project, website and author URLs (capabilities/default.json).
  const repository = REPOSITORY_URL;
  const open = (url: string) => {
    model.platform.openUrl(url).catch((error: unknown) => model.status.error(error, { notify: tr("The link could not be opened") }));
  };
  return (
    <AppModal opened onClose={onClose} title={tr("About BucketBridge")} size="lg">
      <Stack gap="sm">
        <Group justify="space-between" align="flex-start">
          <Stack gap={6}>
            <BrandLogo height={36} withWordmark />
            <Title order={3} className="bb-visually-hidden">
              {info?.name ?? "BucketBridge"}
            </Title>
            <Text size="sm" c="dimmed">
              {tr("Version {version} · {license} license", {
                version: info?.version ?? tr("unknown"),
                license: info?.license ?? "GPL-3.0-only",
              })}
            </Text>
          </Stack>
        </Group>
        <Text size="sm">
          {tr("An open source S3 client for Windows, macOS and Linux. It is free to use: no activation, registration or license key is required, and it never contacts any server on its own. Checking for updates only opens the releases page in your browser.")}
        </Text>
        <Group gap="xs">
          <Button size="xs" variant="default" leftSection={<IconBrandGithub size={14} />} onClick={() => open(repository)}>
            {tr("Source code")}
          </Button>
          <Button size="xs" variant="default" leftSection={<IconBook size={14} />} onClick={() => open(`${repository}/tree/main/docs`)}>
            {tr("Online help")}
          </Button>
          <Button size="xs" variant="default" leftSection={<IconBug size={14} />} onClick={() => open(`${repository}/issues`)}>
            {tr("Contact and issues")}
          </Button>
          <Button size="xs" variant="default" leftSection={<IconRefresh size={14} />} onClick={() => open(`${repository}/releases`)}>
            {tr("Check for updates")}
          </Button>
        </Group>
        <Divider label={tr("Author")} labelPosition="left" />
        <Group justify="space-between" align="center" wrap="nowrap">
          <Stack gap={0}>
            <Text size="sm" fw={600}>
              {tr("Created and maintained by {name}", { name: AUTHOR_NAME })}
            </Text>
            <Text size="xs" c="dimmed">
              {tr("@{handle} on GitHub · Copyright (C) 2026 {name} ({handle})", { handle: AUTHOR_HANDLE, name: AUTHOR_NAME })}
            </Text>
          </Stack>
        </Group>
        <Group gap="xs">
          <Button size="xs" variant="light" leftSection={<IconUser size={14} />} onClick={() => open(AUTHOR_URL)}>
            {tr("{name} on GitHub", { name: AUTHOR_NAME })}
          </Button>
          <Button size="xs" variant="light" leftSection={<IconBrandLinkedin size={14} />} onClick={() => open(LINKEDIN_URL)}>
            LinkedIn
          </Button>
          <Button size="xs" variant="light" leftSection={<IconWorld size={14} />} onClick={() => open(WEBSITE_URL)}>
            {tr("Website")}
          </Button>
          <Button size="xs" variant="light" color="pink" leftSection={<IconHeart size={14} />} onClick={() => open(SPONSOR_URL)}>
            {tr("Sponsor")}
          </Button>
        </Group>
        <Text size="xs" c="dimmed">
          {tr("If BucketBridge is useful to you, a star on GitHub helps others find it.")}
        </Text>
        <Text size="xs" c="dimmed">
          {tr("Repository:")}{" "}
          <Anchor component="button" type="button" size="xs" onClick={() => open(repository)}>
            {repository}
          </Anchor>
        </Text>
        {info !== null ? (
          <Table verticalSpacing={2} fz="xs">
            <Table.Tbody>
              <Table.Tr>
                <Table.Th scope="row" w={140}>
                  {tr("Settings folder")}
                </Table.Th>
                <Table.Td style={{ overflowWrap: "anywhere" }}>{info.config_dir}</Table.Td>
              </Table.Tr>
              <Table.Tr>
                <Table.Th scope="row">{tr("Data folder")}</Table.Th>
                <Table.Td style={{ overflowWrap: "anywhere" }}>{info.data_dir}</Table.Td>
              </Table.Tr>
              <Table.Tr>
                <Table.Th scope="row">{tr("Log folder")}</Table.Th>
                <Table.Td style={{ overflowWrap: "anywhere" }}>{info.log_dir}</Table.Td>
              </Table.Tr>
            </Table.Tbody>
          </Table>
        ) : null}
        <Text size="sm" fw={600}>
          {tr("License")}
        </Text>
        <Code block fz="xs">
          {COPYRIGHT_NOTICE}
        </Code>
        <Text size="xs" c="dimmed">
          {tr("Full text of the GNU General Public License, version 3:")}
        </Text>
        <ScrollArea h={220} type="auto">
          <Code block fz="xs" aria-label="GNU General Public License version 3">
            {LICENSE_TEXT}
          </Code>
        </ScrollArea>
        <Group justify="flex-end">
          <Button onClick={onClose}>{tr("Close")}</Button>
        </Group>
      </Stack>
    </AppModal>
  );
}
