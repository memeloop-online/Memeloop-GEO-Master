import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { FluentProvider, webLightTheme } from "@fluentui/react-components";
import i18n from "../i18n";
import { OperatorAccountImport } from "./OperatorAccountImport";
import {
  createAccountImport,
  getAccountImport,
  retryAccountImportRow,
  type AccountImportResult,
  type AccountImportRequest,
} from "../api/accountImports";

vi.mock("../api/accountImports", () => ({
  createAccountImport: vi.fn(),
  getAccountImport: vi.fn(),
  retryAccountImportRow: vi.fn(),
}));
const result: AccountImportResult = {
  import_key: "batch-reference",
  rows: [
    {
      row_key: "row-one",
      ordinal: 0,
      account_id: "account-one",
      status: "needs_login",
      error_code: null,
      masked_username: "a***",
      display_alias: null,
    },
  ],
};
const onChanged = vi.fn().mockResolvedValue(undefined);
const onLogin = vi.fn();
const accepted = (request: AccountImportRequest): AccountImportResult => ({
  import_key: request.import_key,
  rows: request.rows.map((row, ordinal) => ({
    ...result.rows[0],
    row_key: row.row_key,
    ordinal,
  })),
});
function show() {
  return render(
    <FluentProvider theme={webLightTheme}>
      <OperatorAccountImport
        platforms={[
          {
            id: "zhihu",
            label: "Zhihu",
            purpose: "publishing",
            login_entry_available: true,
            login_supported: true,
            measurement_supported: false,
          },
        ]}
        groups={[]}
        onChanged={onChanged}
        onLogin={onLogin}
        storageScope="synthetic-scope"
      />
    </FluentProvider>,
  );
}
function prepare(text = "username,password\nsynthetic-user,synthetic-secret") {
  fireEvent.change(screen.getByLabelText("Paste CSV here"), {
    target: { value: text },
  });
  fireEvent.click(screen.getByRole("button", { name: "Read and clear list" }));
}
beforeEach(async () => {
  vi.clearAllMocks();
  sessionStorage.clear();
  await i18n.changeLanguage("en");
  vi.mocked(createAccountImport).mockImplementation(async (request) =>
    accepted(request),
  );
});
afterEach(() => cleanup());

describe("operator account import", () => {
  it("imports without a password preview, clears credentials and opens existing login", async () => {
    show();
    prepare();
    expect(screen.getByLabelText("Paste CSV here")).toHaveValue("");
    expect(screen.queryByText("synthetic-secret")).not.toBeInTheDocument();
    expect(screen.getByText("1 accounts prepared")).toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("button", { name: "Import prepared accounts" }),
    );
    await screen.findByText("Needs login");
    expect(screen.getByText("0 accounts prepared")).toBeInTheDocument();
    expect(onChanged).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole("button", { name: "Open login window" }));
    expect(onLogin).toHaveBeenCalledWith("account-one");
    expect(screen.getByLabelText("Import reference")).toHaveValue(
      vi.mocked(createAccountImport).mock.calls[0][0].import_key,
    );
  });

  it("keeps batch and row keys after an uncertain network response", async () => {
    vi.mocked(createAccountImport).mockRejectedValueOnce(
      new Error("synthetic-secret"),
    );
    show();
    prepare();
    fireEvent.click(
      screen.getByRole("button", { name: "Import prepared accounts" }),
    );
    await screen.findByText(/Unable to complete this action/);
    expect(screen.queryByText("synthetic-secret")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Username")).toBeDisabled();
    expect(screen.getByLabelText("Choose CSV file")).toBeDisabled();
    fireEvent.click(
      screen.getByRole("button", { name: "Import prepared accounts" }),
    );
    await screen.findByText("Needs login");
    expect(vi.mocked(createAccountImport).mock.calls[0][0]).toEqual(
      vi.mocked(createAccountImport).mock.calls[1][0],
    );
  });

  it("retries only a failed row without resubmitting credentials", async () => {
    vi.mocked(createAccountImport).mockImplementation(async (request) => ({
      ...accepted(request),
      rows: [
        ...accepted(request).rows,
        {
          ...result.rows[0],
          row_key: "row-two",
          ordinal: 1,
          status: "failed",
          account_id: null,
        },
      ],
    }));
    vi.mocked(retryAccountImportRow).mockResolvedValue(result);
    show();
    prepare();
    fireEvent.click(
      screen.getByRole("button", { name: "Import prepared accounts" }),
    );
    fireEvent.click(await screen.findByRole("button", { name: "Retry row" }));
    await waitFor(() =>
      expect(retryAccountImportRow).toHaveBeenCalledWith(
        vi.mocked(createAccountImport).mock.calls[0][0].import_key,
        "row-two",
      ),
    );
    expect(createAccountImport).toHaveBeenCalledOnce();
  });

  it("retains every original row after an uncertain POST and partial GET", async () => {
    vi.mocked(createAccountImport).mockRejectedValueOnce(new Error("network"));
    show();
    prepare(
      "username,password\nsynthetic-one,synthetic-secret\nsynthetic-two,synthetic-secret",
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Import prepared accounts" }),
    );
    await screen.findByText(/Unable to complete this action/);
    const request = vi.mocked(createAccountImport).mock.calls[0][0];
    vi.mocked(getAccountImport).mockResolvedValue({
      ...accepted(request),
      rows: accepted(request).rows.slice(0, 1),
    });
    fireEvent.click(
      screen.getByText("Import reference", { selector: "summary" }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Retrieve import results" }),
    );
    await screen.findByText("Needs login");
    expect(screen.getByText("2 accounts prepared")).toBeInTheDocument();
    expect(screen.getByLabelText("Username")).toBeDisabled();
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Import prepared accounts" }),
      ).toBeEnabled(),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Import prepared accounts" }),
    );
    await screen.findByText("0 accounts prepared");
    expect(vi.mocked(createAccountImport).mock.calls[1][0]).toEqual(request);
  });

  it("restores a partial import after reload and reuploads the original file with identical keys", async () => {
    const text =
      "username,password\nsynthetic-one,synthetic-secret\nsynthetic-two,synthetic-secret";
    const upload = () => {
      const file = new File([text], "accounts.csv", { type: "text/csv" });
      Object.defineProperty(file, "text", { value: async () => text });
      fireEvent.change(screen.getByLabelText("Choose CSV file"), {
        target: { files: [file] },
      });
    };
    vi.mocked(createAccountImport).mockRejectedValueOnce(new Error("network"));
    const view = show();
    upload();
    await screen.findByText("2 accounts prepared");
    fireEvent.click(
      screen.getByRole("button", { name: "Import prepared accounts" }),
    );
    await screen.findByText(/Unable to complete this action/);
    const original = vi.mocked(createAccountImport).mock.calls[0][0];
    const manifestKey = "account-import-reference:synthetic-scope:pending";
    expect(JSON.parse(sessionStorage.getItem(manifestKey)!)).toEqual({
      import_key: original.import_key,
      row_keys: original.rows.map((row) => row.row_key),
    });
    expect(JSON.stringify(sessionStorage)).not.toContain("synthetic-secret");
    expect(JSON.stringify(sessionStorage)).not.toContain("synthetic-one");
    vi.mocked(getAccountImport).mockResolvedValue({
      ...accepted(original),
      rows: accepted(original).rows.slice(0, 1),
    });
    view.unmount();
    show();
    await screen.findByText(
      "1 of 2 rows saved. Select the original list again to continue.",
    );
    prepare();
    await screen.findByText(
      "Select the original list with all 2 rows in their original order.",
    );
    expect(
      screen.getByRole("button", { name: "Import prepared accounts" }),
    ).toBeDisabled();
    upload();
    await screen.findByText("2 accounts prepared");
    fireEvent.click(
      screen.getByRole("button", { name: "Import prepared accounts" }),
    );
    await screen.findByText("0 accounts prepared");
    expect(vi.mocked(createAccountImport).mock.calls[1][0]).toEqual(original);
    expect(sessionStorage.getItem(manifestKey)).toBeNull();
  });

  it("confirms local clearing without discarding an uncertain import's recovery keys", async () => {
    vi.mocked(createAccountImport).mockRejectedValueOnce(new Error("network"));
    show();
    prepare();
    fireEvent.click(
      screen.getByRole("button", { name: "Import prepared accounts" }),
    );
    await screen.findByText(/Unable to complete this action/);
    const manifestKey = "account-import-reference:synthetic-scope:pending";
    const manifest = sessionStorage.getItem(manifestKey);
    const confirm = vi
      .spyOn(window, "confirm")
      .mockReturnValueOnce(false)
      .mockReturnValueOnce(true);
    fireEvent.click(
      screen.getByRole("button", { name: "Clear prepared accounts" }),
    );
    expect(screen.getByText("1 accounts prepared")).toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("button", { name: "Clear prepared accounts" }),
    );
    expect(screen.getByText("0 accounts prepared")).toBeInTheDocument();
    expect(sessionStorage.getItem(manifestKey)).toBe(manifest);
    expect(confirm).toHaveBeenCalledWith(
      expect.stringContaining("Saved accounts remain"),
    );
    confirm.mockRestore();
  });

  it("discards an old prepared batch as soon as replacement CSV is entered", () => {
    show();
    prepare();
    expect(screen.getByText("1 accounts prepared")).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Paste CSV here"), {
      target: { value: "invalid replacement" },
    });
    expect(
      screen.getByRole("button", { name: "Import prepared accounts" }),
    ).toBeDisabled();
    fireEvent.click(
      screen.getByRole("button", { name: "Read and clear list" }),
    );
    expect(screen.getByText("0 accounts prepared")).toBeInTheDocument();
    expect(createAccountImport).not.toHaveBeenCalled();
  });

  it.each([
    ["username", "界".repeat(171)],
    ["password", "界".repeat(1366)],
    ["display_alias", "界".repeat(54)],
    ["platform", "界".repeat(22)],
  ])(
    "rejects oversized UTF-8 %s without echoing its content",
    (field, value) => {
      show();
      const row: Record<string, string> = {
        username: "synthetic-user",
        password: "synthetic-secret",
        display_alias: "",
        platform: "zhihu",
        [field]: value,
      };
      prepare(
        `username,password,display_alias,platform\n${row.username},${row.password},${row.display_alias},${row.platform}`,
      );
      expect(screen.getByText(/Shorten oversized fields/)).toBeInTheDocument();
      expect(screen.getByText("0 accounts prepared")).toBeInTheDocument();
      expect(screen.queryByText(value)).not.toBeInTheDocument();
      expect(createAccountImport).not.toHaveBeenCalled();
    },
  );

  it("retrieves persistent results by reference after remount", async () => {
    vi.mocked(getAccountImport).mockResolvedValue(result);
    const view = show();
    prepare();
    view.unmount();
    show();
    expect(screen.getByText("0 accounts prepared")).toBeInTheDocument();
    fireEvent.click(
      screen.getByText("Import reference", { selector: "summary" }),
    );
    fireEvent.change(screen.getByLabelText("Import reference"), {
      target: { value: "batch-reference" },
    });
    fireEvent.click(
      screen.getByRole("button", { name: "Retrieve import results" }),
    );
    await screen.findByText("Needs login");
    expect(getAccountImport).toHaveBeenCalledWith("batch-reference");
    expect(screen.getByText(/Only saved rows are shown/)).toBeInTheDocument();
    expect(createAccountImport).not.toHaveBeenCalled();
  });

  it("automatically restores only the scoped nonsecret reference after refresh", async () => {
    vi.mocked(getAccountImport).mockResolvedValue(result);
    const view = show();
    prepare();
    fireEvent.click(
      screen.getByRole("button", { name: "Import prepared accounts" }),
    );
    await screen.findByText("Needs login");
    expect(
      sessionStorage.getItem("account-import-reference:synthetic-scope"),
    ).toBe(vi.mocked(createAccountImport).mock.calls[0][0].import_key);
    expect(JSON.stringify(sessionStorage)).not.toContain("synthetic-secret");
    vi.mocked(getAccountImport).mockResolvedValue(
      accepted(vi.mocked(createAccountImport).mock.calls[0][0]),
    );
    view.unmount();
    show();
    await screen.findByText("Needs login");
    expect(getAccountImport).toHaveBeenCalledWith(
      vi.mocked(createAccountImport).mock.calls[0][0].import_key,
    );
    expect(screen.getByText("0 accounts prepared")).toBeInTheDocument();
    expect(
      screen.queryByText(/Only saved rows are shown/),
    ).not.toBeInTheDocument();
  });

  it("rejects malformed CSV and excessive rows without exposing parser errors", () => {
    show();
    prepare('username,password\n"synthetic-secret');
    expect(screen.getByText(/Unable to read this list/)).toBeInTheDocument();
    expect(screen.getByLabelText("Paste CSV here")).toHaveValue("");
    prepare(
      "username,password\n" +
        Array.from(
          { length: 101 },
          () => "synthetic-user,synthetic-secret",
        ).join("\n"),
    );
    expect(screen.getByText("0 accounts prepared")).toBeInTheDocument();
    expect(createAccountImport).not.toHaveBeenCalled();
  });

  it("reads a bounded CSV file and rejects malformed files", async () => {
    show();
    const file = new File(
      ["username,password\nsynthetic-user,synthetic-secret"],
      "accounts.csv",
      { type: "text/csv" },
    );
    Object.defineProperty(file, "text", {
      value: async () => "username,password\nsynthetic-user,synthetic-secret",
    });
    fireEvent.change(screen.getByLabelText("Choose CSV file"), {
      target: { files: [file] },
    });
    await screen.findByText("1 accounts prepared");
    const invalid = new File(["malformed"], "accounts.csv", {
      type: "text/csv",
    });
    Object.defineProperty(invalid, "text", { value: async () => "malformed" });
    fireEvent.change(screen.getByLabelText("Choose CSV file"), {
      target: { files: [invalid] },
    });
    await screen.findByText(/Unable to read this list/);
    expect(screen.getByText("0 accounts prepared")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Import prepared accounts" }),
    ).toBeDisabled();
    expect(screen.queryByText("synthetic-secret")).not.toBeInTheDocument();
  });

  it("supports adding a row with a masked password field", () => {
    show();
    fireEvent.change(screen.getByLabelText("Username"), {
      target: { value: "synthetic-user" },
    });
    const password = screen.getByLabelText("Password");
    expect(password).toHaveAttribute("type", "password");
    fireEvent.change(password, { target: { value: "synthetic-secret" } });
    fireEvent.click(screen.getByRole("button", { name: "Add row" }));
    expect(password).toHaveValue("");
    expect(screen.getByText("1 accounts prepared")).toBeInTheDocument();
  });
});
