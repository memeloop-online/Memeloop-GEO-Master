import { useAppearance } from "../appearance/AppearanceProvider";

/** Deploy-wide operator branding; never draw from a customer's project. */
export function Brand({ collapsed = false }: { collapsed?: boolean }) {
  const { appearance } = useAppearance();
  const mark =
    appearance.display_name.trim().charAt(0).toLocaleUpperCase() || "G";
  return (
    <div className="brand" aria-label={appearance.display_name}>
      {appearance.logo_url?.startsWith("/") &&
      !appearance.logo_url.startsWith("//") ? (
        <img
          src={appearance.logo_url}
          alt=""
          className="brand-mark"
          referrerPolicy="no-referrer"
        />
      ) : (
        <div className="brand-mark" aria-hidden="true">
          {mark}
        </div>
      )}
      {!collapsed && <span>{appearance.display_name}</span>}
    </div>
  );
}
