# The launcher entry and icon both packages install, matching the Tauri bundle's.
{ makeDesktopItem }:
{
  desktopItem = makeDesktopItem {
    name = "ps-learning-harness";
    desktopName = "P's Learning Harness";
    comment = "A tutor that measures, plans, teaches and tracks what you can do";
    exec = "ps-learning-harness";
    icon = "ps-learning-harness";
    categories = [ "Education" ];
    startupWMClass = "ps-learning-harness";
  };

  installIcon = ''
    install -Dm644 ${../crates/app/icons/128x128.png} \
      $out/share/icons/hicolor/128x128/apps/ps-learning-harness.png
  '';
}
