project(
  'sanctum',
  'rust',
  version: '1.0.5',
  license: 'GPL-3.0-or-later',
  meson_version: '>= 1.0.0',
)

i18n = import('i18n')
gnome = import('gnome')

base_id = 'net.dunevent.Sanctum'

dependency('glib-2.0', version: '>= 2.84')
dependency('gio-2.0', version: '>= 2.84')
dependency('gtk4', version: '>= 4.18')
dependency('libadwaita-1', version: '>= 1.7')
dependency('libsoup-3.0', version: '>= 3.6')
dependency('libsecret-1', version: '>= 0.21')

glib_compile_resources = find_program('glib-compile-resources', required: true)
glib_compile_schemas = find_program('glib-compile-schemas', required: true)
desktop_file_validate = find_program('desktop-file-validate', required: false)
appstreamcli = find_program('appstreamcli', required: false)
cargo = find_program('cargo', required: true)

version = meson.project_version()

# Cargo and the metainfo repeat the version, so setup stops until they match.
fs = import('fs')
cargo_version = ''
foreach line : fs.read('Cargo.toml').split('\n')
  if cargo_version == '' and line.startswith('version = "')
    cargo_version = line.split('"')[1]
  endif
endforeach
lock_version = ''
after_name = false
foreach line : fs.read('Cargo.lock').split('\n')
  if after_name and line.startswith('version = "')
    lock_version = line.split('"')[1]
  endif
  after_name = line == 'name = "@0@"'.format(meson.project_name())
endforeach
release_version = ''
foreach line : fs.read('data' / base_id + '.metainfo.xml.in.in').split('\n')
  if release_version == '' and line.strip().startswith('<release ') and line.contains('version="')
    release_version = line.split('version="')[1].split('"')[0]
  endif
endforeach
foreach file, found : {
  'Cargo.toml': cargo_version,
  'Cargo.lock': lock_version,
  'The newest metainfo release': release_version,
}
  if found != version
    error('@0@ has version "@1@", but meson.build has "@2@"'.format(file, found, version))
  endif
endforeach

prefix = get_option('prefix')
bindir = prefix / get_option('bindir')
localedir = prefix / get_option('localedir')
datadir = prefix / get_option('datadir')
pkgdatadir = datadir / meson.project_name()
iconsdir = datadir / 'icons'
podir = meson.project_source_root() / 'po'
gettext_package = meson.project_name()

if get_option('profile') == 'development'
  profile = 'Devel'
  vcs_tag = run_command('git', 'rev-parse', '--short', 'HEAD', check: false).stdout().strip()
  if vcs_tag == ''
    version_suffix = '-devel'
  else
    version_suffix = '-@0@'.format(vcs_tag)
  endif
else
  profile = ''
  version_suffix = ''
endif
application_id = base_id

meson.add_dist_script(
  'build-aux/dist-vendor.sh',
  meson.project_build_root() / 'meson-dist' / meson.project_name() + '-' + version,
  meson.project_source_root(),
)

subdir('data')
subdir('po')
subdir('src')

gnome.post_install(
  gtk_update_icon_cache: true,
  glib_compile_schemas: true,
  update_desktop_database: true,
)
