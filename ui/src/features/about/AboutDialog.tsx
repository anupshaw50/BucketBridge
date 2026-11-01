import { Anchor, Button, Code, Group, ScrollArea, Stack, Table, Text, Title } from "@mantine/core";
import { IconBrandGithub, IconBook, IconBug, IconRefresh } from "@tabler/icons-react";
import { useModel, useSlice } from "../../app/context";
import { AppModal } from "../../components/AppModal";

export const REPOSITORY_URL = "https://github.com/anupshaw50/BucketBridge";

const MIT_LICENSE = `MIT License

Copyright (c) 2026 The BucketBridge authors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.`;

/** Version, license, links, and the statement that nothing needs activating. */
export function AboutDialog({ onClose }: { onClose: () => void }) {
  const model = useModel();
  const info = useSlice(model.app, (state) => state.info);
  // Links always use the project URL: the desktop shell only allows opening URLs below it.
  const repository = REPOSITORY_URL;
  const open = (url: string) => {
    model.platform.openUrl(url).catch((error: unknown) => model.status.error(error, { notify: "The link could not be opened" }));
  };
  return (
    <AppModal opened onClose={onClose} title="About BucketBridge" size="lg">
      <Stack gap="sm">
        <Group justify="space-between" align="flex-start">
          <Stack gap={2}>
            <Title order={3}>{info?.name ?? "BucketBridge"}</Title>
            <Text size="sm" c="dimmed">
              Version {info?.version ?? "unknown"} · {info?.license ?? "MIT"} license
            </Text>
          </Stack>
        </Group>
        <Text size="sm">
          An open source S3 client for Windows, macOS and Linux. It is free to use: no activation, registration or
          license key is required, and it never contacts any server on its own. Checking for updates only opens the
          releases page in your browser.
        </Text>
        <Group gap="xs">
          <Button size="xs" variant="default" leftSection={<IconBrandGithub size={14} />} onClick={() => open(repository)}>
            Source code
          </Button>
          <Button size="xs" variant="default" leftSection={<IconBook size={14} />} onClick={() => open(`${repository}/tree/main/docs`)}>
            Online help
          </Button>
          <Button size="xs" variant="default" leftSection={<IconBug size={14} />} onClick={() => open(`${repository}/issues`)}>
            Contact and issues
          </Button>
          <Button size="xs" variant="default" leftSection={<IconRefresh size={14} />} onClick={() => open(`${repository}/releases`)}>
            Check for updates
          </Button>
        </Group>
        <Text size="xs" c="dimmed">
          Repository:{" "}
          <Anchor component="button" type="button" size="xs" onClick={() => open(repository)}>
            {repository}
          </Anchor>
        </Text>
        {info !== null ? (
          <Table verticalSpacing={2} fz="xs">
            <Table.Tbody>
              <Table.Tr>
                <Table.Th scope="row" w={140}>
                  Settings folder
                </Table.Th>
                <Table.Td style={{ overflowWrap: "anywhere" }}>{info.config_dir}</Table.Td>
              </Table.Tr>
              <Table.Tr>
                <Table.Th scope="row">Data folder</Table.Th>
                <Table.Td style={{ overflowWrap: "anywhere" }}>{info.data_dir}</Table.Td>
              </Table.Tr>
              <Table.Tr>
                <Table.Th scope="row">Log folder</Table.Th>
                <Table.Td style={{ overflowWrap: "anywhere" }}>{info.log_dir}</Table.Td>
              </Table.Tr>
            </Table.Tbody>
          </Table>
        ) : null}
        <Text size="sm" fw={600}>
          License
        </Text>
        <ScrollArea h={180} type="auto">
          <Code block fz="xs">
            {MIT_LICENSE}
          </Code>
        </ScrollArea>
        <Group justify="flex-end">
          <Button onClick={onClose}>Close</Button>
        </Group>
      </Stack>
    </AppModal>
  );
}
