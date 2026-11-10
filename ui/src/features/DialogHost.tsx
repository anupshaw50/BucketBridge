import { Fragment } from "react";
import { useModel, useSlice } from "../app/context";
import { ConfirmDialog } from "../components/ConfirmDialog";
import type { OpenDialog } from "../state/dialogs";
import { AboutDialog } from "./about/AboutDialog";
import { AccountManagerDialog, AddAccountDialog, EditAccountDialog } from "./accounts/AccountDialogs";
import { AccountFolderDialog } from "./accounts/AccountFolderDialog";
import { ImportAccountsDialog } from "./accounts/ImportAccountsDialog";
import { ImportAwsDialog } from "./accounts/ImportAwsDialog";
import { MasterPasswordDialog } from "./accounts/MasterPasswordDialog";
import { CreateBucketDialog, DeleteBucketDialog } from "./buckets/BucketDialogs";
import { BookmarkDialog } from "./bookmarks/BookmarkDialog";
import { BucketPropertiesDialog } from "./bucketcfg/BucketPropertiesDialog";
import { BucketRulesDialog } from "./bucketrules/BucketRulesDialog";
import { ExternalBucketDialog } from "./bucketcfg/ExternalBucketDialog";
import { CloudFrontDialog } from "./cloudfront/CloudFrontDialog";
import { CrashReportDialog } from "./crash/CrashReportDialog";
import { QuitDialog } from "./quit/QuitDialog";
import { DestinationPicker } from "./picker/DestinationPicker";
import { NewFolderDialog, RenameDialog, ShareLinkDialog } from "./objects/ObjectDialogs";
import { BucketLockDialog } from "./objectlock/BucketLockDialog";
import { ObjectLockDialog } from "./objectlock/ObjectLockDialog";
import { OwnershipDialog } from "./objectlock/OwnershipDialog";
import { RestoreDialog } from "./objectlock/RestoreDialog";
import { IamDialog } from "./iam/IamDialog";
import { EncryptionDialog } from "./cse/EncryptionDialog";
import { EditConflictDialog } from "./edit/EditConflictDialog";
import { EditingDialog } from "./edit/EditingDialog";
import { OpenWithDialog } from "./fileopen/OpenWithDialog";
import { WebsiteRedirectDialog } from "./fileopen/WebsiteRedirectDialog";
import { MetadataDialog } from "./objectmeta/MetadataDialog";
import { MountDialog } from "./mount/MountDialog";
import { SecurityScanDialog } from "./scan/SecurityScanDialog";
import { CloneBucketDialog } from "./bucketclone/CloneBucketDialog";
import { SharingWizard } from "./sharing/SharingWizard";
import { SettingsDialog } from "./settings/SettingsDialog";
import { SyncDialog } from "./sync/SyncDialog";
import { SyncJobsDialog } from "./sync/SyncJobsDialog";
import { DiagnosticsDialog } from "./tools/DiagnosticsDialog";
import { ExportDialog } from "./tools/ExportDialog";
import { FiltersDialog } from "./tools/FiltersDialog";
import { UploadsDialog } from "./tools/UploadsDialog";
import { PreviewDialog } from "./versions/PreviewDialog";
import { VersionsDialog } from "./versions/VersionsDialog";

function renderDialog(dialog: OpenDialog, close: () => void, answer: (confirmed: boolean) => void) {
  switch (dialog.kind) {
    case "accounts":
      return <AccountManagerDialog onClose={close} />;
    case "add-account":
      return <AddAccountDialog onClose={close} />;
    case "import-aws":
      return <ImportAwsDialog onClose={close} />;
    case "import-accounts":
      return <ImportAccountsDialog onClose={close} />;
    case "account-folder":
      return <AccountFolderDialog folder={dialog.folder} onClose={close} />;
    case "edit-account":
      return <EditAccountDialog accountId={dialog.accountId} focusMfa={dialog.focus === "mfa"} onClose={close} />;
    case "master-password":
      return <MasterPasswordDialog onClose={close} />;
    case "settings":
      return <SettingsDialog initialPage={dialog.page} onClose={close} />;
    case "about":
      return <AboutDialog onClose={close} />;
    case "sync":
      return <SyncDialog at={dialog.at} job={dialog.job} review={dialog.review} onClose={close} />;
    case "sync-jobs":
      return <SyncJobsDialog onClose={close} />;
    case "create-bucket":
      return <CreateBucketDialog accountId={dialog.accountId} onClose={close} />;
    case "delete-bucket":
      return <DeleteBucketDialog accountId={dialog.accountId} bucket={dialog.bucket} onClose={close} />;
    case "bucket-properties":
      return <BucketPropertiesDialog accountId={dialog.accountId} bucket={dialog.bucket} initialTab={dialog.tab} onClose={close} />;
    case "bucket-rules":
      return <BucketRulesDialog accountId={dialog.accountId} bucket={dialog.bucket} initialTab={dialog.tab} onClose={close} />;
    case "external-bucket":
      return <ExternalBucketDialog accountId={dialog.accountId} onClose={close} />;
    case "bookmark":
      return (
        <BookmarkDialog
          accountId={dialog.accountId}
          bucket={dialog.bucket}
          prefix={dialog.prefix}
          bookmarkId={dialog.bookmarkId}
          label={dialog.label}
          onClose={close}
        />
      );
    case "new-folder":
      return <NewFolderDialog accountId={dialog.accountId} bucket={dialog.bucket} prefix={dialog.prefix} onClose={close} />;
    case "rename":
      return <RenameDialog accountId={dialog.accountId} bucket={dialog.bucket} objectKey={dialog.key} onClose={close} />;
    case "properties":
      return (
        <MetadataDialog
          accountId={dialog.accountId}
          bucket={dialog.bucket}
          entries={dialog.entries}
          initialTab={dialog.tab}
          onClose={close}
        />
      );
    case "encryption":
      return (
        <EncryptionDialog
          mode={dialog.mode}
          accountId={dialog.accountId}
          bucket={dialog.bucket}
          keys={dialog.keys}
          onClose={close}
        />
      );
    case "share":
      return <ShareLinkDialog accountId={dialog.accountId} bucket={dialog.bucket} objectKey={dialog.key} onClose={close} />;
    case "diagnostics":
      return <DiagnosticsDialog accountId={dialog.accountId} onClose={close} />;
    case "mount":
      return <MountDialog accountId={dialog.accountId} bucket={dialog.bucket} onClose={close} />;
    case "security-scan":
      return <SecurityScanDialog onClose={close} />;
    case "uploads":
      return <UploadsDialog onClose={close} />;
    case "iam":
      return <IamDialog onClose={close} />;
    case "filters":
      return <FiltersDialog onClose={close} />;
    case "destination-picker":
      return (
        <DestinationPicker
          mode={dialog.mode}
          accountId={dialog.accountId}
          sourceBucket={dialog.bucket}
          keys={dialog.keys}
          onClose={close}
        />
      );
    case "export":
      return <ExportDialog accountId={dialog.accountId} bucket={dialog.bucket} prefix={dialog.prefix} onClose={close} />;
    case "versions":
      return (
        <VersionsDialog
          accountId={dialog.accountId}
          bucket={dialog.bucket}
          prefix={dialog.prefix}
          exact={dialog.exact}
          onClose={close}
        />
      );
    case "cloudfront":
      return <CloudFrontDialog accountId={dialog.accountId} bucket={dialog.bucket} view={dialog.view} onClose={close} />;
    case "preview":
      return (
        <PreviewDialog
          accountId={dialog.accountId}
          bucket={dialog.bucket}
          objectKey={dialog.key}
          versionId={dialog.versionId}
          onClose={close}
        />
      );
    case "sharing-wizard":
      return <SharingWizard accountId={dialog.accountId} bucket={dialog.bucket} onClose={close} />;
    case "bucket-clone":
      return <CloneBucketDialog accountId={dialog.accountId} bucket={dialog.bucket} onClose={close} />;
    case "bucket-lock":
      return <BucketLockDialog accountId={dialog.accountId} bucket={dialog.bucket} onClose={close} />;
    case "bucket-ownership":
      return <OwnershipDialog accountId={dialog.accountId} bucket={dialog.bucket} onClose={close} />;
    case "object-lock":
      return <ObjectLockDialog accountId={dialog.accountId} bucket={dialog.bucket} keys={dialog.keys} onClose={close} />;
    case "object-restore":
      return <RestoreDialog accountId={dialog.accountId} bucket={dialog.bucket} keys={dialog.keys} onClose={close} />;
    case "open-with":
      return <OpenWithDialog accountId={dialog.accountId} bucket={dialog.bucket} objectKey={dialog.key} onClose={close} />;
    case "website-redirect":
      return <WebsiteRedirectDialog accountId={dialog.accountId} bucket={dialog.bucket} objectKey={dialog.key} onClose={close} />;
    case "edit-sessions":
      return <EditingDialog onClose={close} />;
    case "edit-conflict":
      return <EditConflictDialog sessionId={dialog.sessionId} onClose={close} />;
    case "crash":
      return <CrashReportDialog report={dialog.report} onClose={close} />;
    case "quit":
      return <QuitDialog request={dialog.request} onClose={close} />;
    case "confirm":
      return <ConfirmDialog options={dialog.options} onAnswer={answer} />;
  }
}

/** Renders every open dialog, bottom-most first. */
export function DialogHost() {
  const model = useModel();
  const open = useSlice(model.dialogs.store, (state) => state.open);
  return (
    <>
      {open.map((dialog) => (
        <Fragment key={dialog.id}>
          {renderDialog(
            dialog,
            () => model.dialogs.close(dialog.id),
            (confirmed) => model.dialogs.answer(dialog.id, confirmed),
          )}
        </Fragment>
      ))}
    </>
  );
}
