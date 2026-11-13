import hashlib, importlib.util, json, pathlib, tempfile, unittest, zipfile

class BrowserArtifactsTest(unittest.TestCase):
    def test_cached_archive_must_match_pin_before_extraction(self):
        spec=importlib.util.spec_from_file_location('browser',pathlib.Path(__file__).with_name('prepare-browser.py'))
        module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as directory:
            root=pathlib.Path(directory);cache=root/'cache';cache.mkdir()
            archive=cache/'linux64.zip'
            with zipfile.ZipFile(archive,'w') as z:z.writestr('chrome-headless-shell-linux64/chrome-headless-shell','fixture browser')
            asset={'platform':'linux64','url':'https://example.invalid/browser.zip','sha256':hashlib.sha256(archive.read_bytes()).hexdigest(),'files':{'chrome-headless-shell':hashlib.sha256(b'fixture browser').hexdigest()}}
            manifest={'version':'fixture','assets':[asset]}
            module.prepare(manifest,'linux64',cache,root/'browser',download=False)
            self.assertEqual((root/'browser/chrome-headless-shell').read_text(),'fixture browser')
            self.assertEqual(json.loads((root/'browser/brr-browser-manifest.json').read_text()),{'version':'fixture',**asset})
            archive.write_bytes(b'tampered')
            with self.assertRaisesRegex(ValueError,'SHA-256'):
                module.prepare(manifest,'linux64',cache,root/'browser',download=False)
            self.assertEqual((root/'browser/chrome-headless-shell').read_text(),'fixture browser')

if __name__=='__main__':unittest.main()
