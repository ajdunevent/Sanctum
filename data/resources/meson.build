blueprints = custom_target(
  'blueprints',
  input: files(
    'ui/assist-window.blp',
    'ui/onboarding.blp',
    'ui/preferences-dialog.blp',
    'ui/window.blp',
  ),
  # Each .ui is listed so editing a .blp rebuilds the resource bundle. With
  # the directory as the output, incremental builds keep the old templates.
  output: [
    'assist-window.ui',
    'onboarding.ui',
    'preferences-dialog.ui',
    'window.ui',
  ],
  command: [
    find_program('blueprint-compiler'),
    'batch-compile',
    '@OUTDIR@',
    '@CURRENT_SOURCE_DIR@' / 'ui',
    '@INPUT@',
  ],
)

resources = gnome.compile_resources(
  'resources',
  'resources.gresource.xml',
  gresource_bundle: true,
  source_dir: [meson.current_build_dir(), meson.current_source_dir()],
  install: true,
  install_dir: pkgdatadir,
  dependencies: blueprints,
)

mdi_resources = gnome.compile_resources(
  'mdi',
  'mdi.gresource.xml',
  gresource_bundle: true,
  source_dir: meson.current_source_dir(),
  install: true,
  install_dir: pkgdatadir,
)
