#!/usr/bin/env ruby
require 'xcodeproj'

PROJECT_PATH = '/Users/you/Herald-build108/Herald.xcodeproj'

project = Xcodeproj::Project.open(PROJECT_PATH)

def ensure_swift(project:, target:, group:, path:, relative_to:)
  existing = group.files.find { |f| f.path == File.basename(path) }
  return existing if existing
  absolute = path
  refs = project.main_group.recursive_children_groups.select { |g| g.path == File.basename(File.dirname(absolute)) || g.path == relative_to }
  ref = group.new_reference(File.basename(absolute))
  ref.path = File.basename(absolute)
  ref.last_known_file_type = 'sourcecode.swift'
  target.add_file_references([ref])
  project.targets.each { |t| t.source_build_phase.add_file_reference(ref, true) unless t == target || t.source_build_phase.files_references.include?(ref) }
  ref
end

herald_target = project.targets.find { |t| t.name == 'Herald' }
tests_target = project.targets.find { |t| t.name == 'HeraldTests' }

models_group = herald_target.project.main_group['Herald']['Models']
stores_group = herald_target.project.main_group['Herald']['Stores']
tests_group = herald_target.project.main_group['HeraldTests']

# remove any stale references that lacked build references
['TranscriptIdentity.swift', 'TranscriptRow.swift'].each do |file|
  existing = models_group.files.find { |f| f.path == file }
  next unless existing
  ref_to_use = existing
  herald_target.source_build_phase.remove_file_reference(ref_to_use) if ref_to_use
  models_group.children.delete(ref_to_use) if ref_to_use.respond_to?(:parent)
  project.objects[ref_to_use.uuid].remove_from_project if project.objects[ref_to_use.uuid]
end

%w[TranscriptIdentity TranscriptRow].each do |name|
  ref = models_group.new_reference("#{name}.swift")
  ref.last_known_file_type = 'sourcecode.swift'
  herald_target.add_file_references([ref])
end

reducer_ref = stores_group.new_reference('TranscriptReducer.swift')
reducer_ref.last_known_file_type = 'sourcecode.swift'
herald_target.add_file_references([reducer_ref])

# Tests
test_ref = tests_group.new_reference('TranscriptReducerTests.swift')
test_ref.last_known_file_type = 'sourcecode.swift'
tests_target.add_file_references([test_ref])

project.save
puts 'ok'
